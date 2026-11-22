#!/usr/bin/env node
/**
 * check_idl.mjs — verify the HAND-MAINTAINED IDL against the program source
 * and the JS SDK. `anchor build` cannot generate an IDL on any installed
 * toolchain (ark-bn254 MontFp macro panics on 1.81/1.82/1.85; 1.79 is too
 * old for wasm-bindgen), so idl/agentpad.json is edited by hand and this
 * script is the check that the edit stayed consistent.
 *
 *   node scripts/check_idl.mjs            (exit 0 = all checks pass)
 *
 * Checks:
 *  1. program address: IDL metadata == shared/constants.json == Anchor.toml
 *  2. instruction set: IDL == lib.rs `pub fn` handlers == SDK DISCRIMINATORS
 *  3. discriminators: sha256("global:<ix>")[0:8] == IDL array == SDK hex
 *  4. accounts per instruction: ordered {name,isMut,isSigner,isOptional}
 *     from the #[derive(Accounts)] struct vs the IDL entry. Mutability comes
 *     ONLY from `mut` / `init` / `init_if_needed` in the #[account(...)]
 *     attribute (multi-line aware); Signer and AccountInfo/UncheckedAccount
 *     do NOT imply mut. Four handler-driven flags live in FLAG_EXCEPTIONS
 *     with their evidence (the struct cannot express them).
 *  5. instruction args: names + types from the handler signature vs IDL.
 *  6. errors: errors.rs enum order (code = 6000 + index), names and #[msg]
 *     strings vs the IDL error list.
 *  7. account discriminators: sha256("account:<Name>")[0:8] == SDK map.
 *  8. account types: IDL `accounts` fields vs state.rs structs.
 *
 * A mismatch here means a client built from the IDL (or the SDK) would
 * pass accounts/args in the wrong order or with the wrong writability and
 * the program would misinterpret the call — check before every deploy.
 */
import fs from 'node:fs';
import crypto from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = (p) => fs.readFileSync(path.join(ROOT, p), 'utf8');
// Layout differs between the private repo (agentpad_phase3/ prefix) and the
// public repo (flat: programs/, Anchor.toml at root). Prefer the prefixed path.
const resolve = (p) =>
  fs.existsSync(path.join(ROOT, 'agentpad_phase3', p)) ? path.join('agentpad_phase3', p) : p;
const idl = JSON.parse(read('idl/agentpad.json'));
const lib = read(resolve('programs/agentpad/src/lib.rs'));
const errorsRs = read(resolve('programs/agentpad/src/errors.rs'));
const stateRs = read(resolve('programs/agentpad/src/state.rs'));
const sdk = read('sdk/agentpad-sdk.mjs');
const constants = JSON.parse(read('shared/constants.json'));
const anchorToml = read(resolve('Anchor.toml'));

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? ' — ' + detail : ''}`);
}
const sha8 = (s) => crypto.createHash('sha256').update(s).digest().subarray(0, 8);
const hex = (b) => Buffer.from(b).toString('hex');
const bytesEq = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);

// ---- 1. program address ----
const tomlIds = [...anchorToml.matchAll(/^agentpad\s*=\s*"([^"]+)"/gm)].map((m) => m[1]);
check(
  '1. program address',
  idl.metadata.address === constants.programId && tomlIds.every((x) => x === constants.programId),
  `idl=${idl.metadata.address} constants=${constants.programId} anchor.toml=[${[...new Set(tomlIds)].join(',')}]`,
);

// ---- 2. instruction set ----
const idlIxNames = idl.instructions.map((i) => i.name);
const sdkDisc = Object.fromEntries(
  [...sdk.matchAll(/export const DISCRIMINATORS = \{([\s\S]*?)\};/g)].flatMap((m) =>
    [...m[1].matchAll(/(\w+):\s*'([0-9a-f]+)'/g)].map((x) => [x[1], x[2]]),
  ),
);
// lib.rs handlers: `pub fn name(` inside the #[program] module
const handlerNames = [...lib.matchAll(/pub fn (\w+)\s*\(/g)].map((m) => m[1]);
const setEq = (a, b) => a.length === b.length && [...a].sort().join() === [...b].sort().join();
check(
  '2. instruction set (IDL == lib.rs == SDK)',
  setEq(idlIxNames, handlerNames) && setEq(idlIxNames, Object.keys(sdkDisc)),
  `idl=${idlIxNames.length} lib=${handlerNames.length} sdk=${Object.keys(sdkDisc).length}`,
);

// ---- 3. discriminators ----
let discOk = true;
const discBad = [];
for (const ix of idl.instructions) {
  const want = sha8('global:' + ix.name);
  if (!bytesEq(ix.discriminator, [...want])) { discOk = false; discBad.push(ix.name + ':idl'); }
  if (sdkDisc[ix.name] !== hex(want)) { discOk = false; discBad.push(ix.name + ':sdk'); }
}
check('3. instruction discriminators (IDL + SDK)', discOk, discBad.join(', ') || `${idlIxNames.length} instructions`);

// ---- helpers: parse Rust structs ----
function parseStructs(src) {
  const out = new Map();
  const re = /pub struct (\w+)(?:<'info>)?\s*\{/g;
  let m;
  while ((m = re.exec(src))) {
    const name = m[1];
    let i = re.lastIndex;
    let depth = 1;
    const start = i;
    while (i < src.length && depth > 0) {
      if (src[i] === '{') depth++;
      if (src[i] === '}') depth--;
      i++;
    }
    out.set(name, src.slice(start, i - 1));
    re.lastIndex = i;
  }
  return out;
}
function parseFields(body) {
  // Attributes may span several lines (e.g. #[account(\n init,\n ...\n )]);
  // accumulate them until the brackets balance, then attach to the next field.
  const fields = [];
  const lines = body.split('\n');
  let attrBuf = '';
  let attrDepth = 0;
  let inAttr = false;
  for (const raw of lines) {
    const line = raw.trim();
    if (!line || line.startsWith('//')) continue;
    if (line.startsWith('#[')) {
      inAttr = true;
      attrBuf = line;
      attrDepth = (line.match(/\[/g) || []).length - (line.match(/\]/g) || []).length;
      if (attrDepth <= 0) inAttr = false;
      continue;
    }
    if (inAttr) {
      attrBuf += ' ' + line;
      attrDepth += (line.match(/\[/g) || []).length - (line.match(/\]/g) || []).length;
      if (attrDepth <= 0) inAttr = false;
      continue;
    }
    const fm = line.match(/^pub\s+(\w+)\s*:\s*(.+?)\s*,?\s*$/);
    if (fm) {
      fields.push({ name: fm[1], type: fm[2].replace(/,$/, ''), attrs: attrBuf });
      attrBuf = '';
    }
  }
  return fields;
}
const unwrap = (t) => {
  let s = t.trim();
  while (s.startsWith('Box<') && s.endsWith('>')) s = s.slice(4, -1);
  return s;
};
const isOptionalType = (t) => unwrap(t).startsWith('Option<');
const innerType = (t) => {
  const s = unwrap(t);
  const m = s.match(/^Option<(.+)>$/);
  return m ? m[1] : s;
};
function expectedFlags(f) {
  // Mirrors what the IDL carries for this codebase (verified against the
  // last machine-generated IDL and the live program):
  //  - isMut comes ONLY from `mut` / `init` / `init_if_needed` in the
  //    #[account(...)] attribute. Signer does NOT imply mut (ClaimTokens.user
  //    is a plain Signer, IDL mut=false; ClaimDividend.user has #[account(mut)]).
  //  - AccountInfo / UncheckedAccount do NOT imply mut either
  //    (CreateRaydiumPool.mint is UncheckedAccount, IDL mut=false).
  //  - isSigner is true only for Signer<'info>.
  //  - isOptional is true only for Option<...>.
  // Four flags cannot be derived from the struct at all — the handler needs
  // them, so they live in FLAG_EXCEPTIONS below with the handler evidence.
  const inner = innerType(f.type);
  const isSigner = inner.startsWith('Signer<');
  const hasMut = /\b(mut|init|init_if_needed)\b/.test(f.attrs);
  return {
    isMut: hasMut,
    isSigner,
    isOptional: isOptionalType(f.type),
  };
}
// Handler-driven flags the struct cannot express. The checker FAILS if the
// IDL stops matching an entry here, so an entry can never silently go stale.
// If a new AccountInfo/UncheckedAccount field needs writable/signer in the
// IDL, add it here WITH the handler evidence (file + line), never silently.
const FLAG_EXCEPTIONS = {
  // Handler creates the record via manual SystemProgram CPI on first claim
  // (v4.2.8); the CPI fails unless the client marks it writable.
  'claim_dividend.minter_record': { isMut: true },
  // Same, second leg (v4.3.1 manual-CPI creation).
  'claim_dividend.minter_reward_2': { isMut: true },
  // Handler creates it via manual CPI when reward_mint_2 is set
  // (lib.rs create_launch, dividend_vault_2 seeds block).
  'create_launch.dividend_vault_2': { isMut: true },
  // Handler requires gs.is_signer && gs.key() == cfg.gate_signer
  // (lib.rs create_launch); an unsigned gate key must not count as official.
  'create_launch.gate_signer': { isSigner: true },
};
const accountsStructs = parseStructs(lib);

// ---- 4. accounts per instruction ----
function handlerInfo(name) {
  // find `pub fn name(` then balance parens to get the full signature
  const idx = lib.indexOf('pub fn ' + name + '(');
  if (idx < 0) return null;
  let i = lib.indexOf('(', idx);
  let depth = 0;
  const start = i;
  while (i < lib.length) {
    if (lib[i] === '(') depth++;
    if (lib[i] === ')') { depth--; if (depth === 0) { i++; break; } }
    i++;
  }
  const sig = lib.slice(start, i);
  const ctxM = sig.match(/ctx\s*:\s*Context<(\w+)>/);
  // split top-level commas
  const params = [];
  let d = 0, cur = '', inStr = false;
  for (const ch of sig.slice(1, -1)) {
    if (ch === '"') inStr = !inStr;
    if (!inStr) {
      if (ch === '<' || ch === '(' || ch === '[') d++;
      if (ch === '>' || ch === ')' || ch === ']') d--;
      if (ch === ',' && d === 0) { params.push(cur); cur = ''; continue; }
    }
    cur += ch;
  }
  if (cur.trim()) params.push(cur);
  const args = params.slice(1).map((p) => {
    const mm = p.trim().match(/^(\w+)\s*:\s*(.+)$/s);
    return mm ? { name: mm[1], type: mm[2].trim() } : null;
  }).filter(Boolean);
  return { structName: ctxM ? ctxM[1] : null, args };
}
let accOk = true;
const accBad = [];
// An exception entry must point at a real instruction.account, or it is stale.
for (const key of Object.keys(FLAG_EXCEPTIONS)) {
  const [ixName, accName] = key.split('.');
  const ix = idl.instructions.find((i) => i.name === ixName);
  if (!ix || !ix.accounts.some((a) => a.name === accName)) {
    accOk = false;
    accBad.push('stale exception: ' + key);
  }
}
for (const ix of idl.instructions) {
  const info = handlerInfo(ix.name);
  if (!info || !info.structName) { accOk = false; accBad.push(ix.name + ':no-handler'); continue; }
  const body = accountsStructs.get(info.structName);
  if (body === undefined) { accOk = false; accBad.push(ix.name + ':no-struct-' + info.structName); continue; }
  const fields = parseFields(body);
  const want = fields.map((f) => {
    const base = { name: f.name, ...expectedFlags(f) };
    const ex = FLAG_EXCEPTIONS[ix.name + '.' + f.name];
    return ex ? { ...base, ...ex } : base;
  });
  const got = ix.accounts.map((a) => ({ name: a.name, isMut: !!a.isMut, isSigner: !!a.isSigner, isOptional: !!a.isOptional }));
  if (want.length !== got.length) { accOk = false; accBad.push(`${ix.name}:count want=${want.length} got=${got.length}`); continue; }
  for (let k = 0; k < want.length; k++) {
    const w = want[k], g = got[k];
    if (w.name !== g.name || w.isMut !== g.isMut || w.isSigner !== g.isSigner || w.isOptional !== g.isOptional) {
      accOk = false;
      accBad.push(`${ix.name}.${w.name}: want mut=${w.isMut} signer=${w.isSigner} opt=${w.isOptional} | idl mut=${g.isMut} signer=${g.isSigner} opt=${g.isOptional}`);
    }
  }
}
check('4. accounts per instruction (order + flags)', accOk, accBad.join('; ') || `${idlIxNames.length} instructions`);

// ---- 5. instruction args ----
const rustToIdl = (t) => {
  const s = t.replace(/\s+/g, '');
  const prim = { u8: 'u8', u16: 'u16', u32: 'u32', u64: 'u64', u128: 'u128', i8: 'i8', i16: 'i16', i32: 'i32', i64: 'i64', i128: 'i128', bool: 'bool', String: 'string', Pubkey: 'pubkey' };
  if (prim[s]) return prim[s];
  const v8 = s.match(/^Vec<u8>$/);
  if (v8) return 'bytes';
  const vec = s.match(/^Vec<(.+)>$/);
  if (vec) return { vec: rustToIdl(vec[1]) };
  const arr = s.match(/^\[(.+);(\d+)\]$/);
  if (arr) return { array: [rustToIdl(arr[1]), Number(arr[2])] };
  const opt = s.match(/^Option<(.+)>$/);
  if (opt) return { option: rustToIdl(opt[1]) };
  return { defined: s }; // named struct/enum
};
const normType = (t) => {
  // Anchor 0.30 writes defined types as {defined:{name,generics:[]}}; normalize to {defined:name}.
  if (t && typeof t === 'object' && t.defined !== undefined) {
    return { defined: typeof t.defined === 'string' ? t.defined : t.defined.name };
  }
  return t;
};
const idlTypeEq = (a, b) => JSON.stringify(normType(a)) === JSON.stringify(normType(b));
let argsOk = true;
const argsBad = [];
for (const ix of idl.instructions) {
  const info = handlerInfo(ix.name);
  if (!info) continue;
  const want = info.args.map((a) => ({ name: a.name, type: rustToIdl(a.type) }));
  const got = (ix.args || []).map((a) => ({ name: a.name, type: a.type }));
  if (want.length !== got.length) { argsOk = false; argsBad.push(`${ix.name}:arg-count want=${want.length} got=${got.length}`); continue; }
  for (let k = 0; k < want.length; k++) {
    if (want[k].name !== got[k].name || !idlTypeEq(want[k].type, got[k].type)) {
      argsOk = false;
      argsBad.push(`${ix.name}.${want[k].name}: want ${JSON.stringify(want[k].type)} got ${JSON.stringify(got[k].type)}`);
    }
  }
}
check('5. instruction args (names + types)', argsOk, argsBad.join('; ') || 'all match');

// ---- 6. errors ----
const errVariants = [...errorsRs.matchAll(/#\[msg\("((?:[^"\\]|\\.)*)"\)\]\s*\n\s*(\w+),/g)].map((m) => ({ name: m[2], msg: m[1] }));
let errOk = true;
const errBad = [];
if (errVariants.length !== idl.errors.length) { errOk = false; errBad.push(`count rust=${errVariants.length} idl=${idl.errors.length}`); }
for (let k = 0; k < Math.min(errVariants.length, idl.errors.length); k++) {
  const w = errVariants[k], g = idl.errors[k];
  if (g.code !== 6000 + k || g.name !== w.name || g.msg !== w.msg) {
    errOk = false;
    errBad.push(`#${k} code=${g.code} name=${g.name}/${w.name} msg-match=${g.msg === w.msg}`);
  }
}
check('6. errors (code = 6000 + index, names, messages)', errOk, errBad.join('; ') || `${idl.errors.length} errors`);

// ---- 7. account discriminators ----
const sdkAccDisc = Object.fromEntries(
  [...sdk.matchAll(/export const ACCOUNT_DISCRIMINATORS = \{([\s\S]*?)\};/g)].flatMap((m) =>
    [...m[1].matchAll(/(\w+):\s*'([0-9a-f]+)'/g)].map((x) => [x[1], x[2]]),
  ),
);
let accDiscOk = true;
const accDiscBad = [];
for (const a of (idl.accounts || [])) {
  if (sdkAccDisc[a.name] !== hex(sha8('account:' + a.name))) { accDiscOk = false; accDiscBad.push(a.name); }
}
check('7. account discriminators (SDK)', accDiscOk, accDiscBad.join(', ') || `${(idl.accounts || []).length} account types`);

// ---- 8. account types vs state.rs ----
const stateStructs = parseStructs(stateRs);
let typeOk = true;
const typeBad = [];
for (const a of (idl.accounts || [])) {
  const body = stateStructs.get(a.name);
  if (body === undefined) { typeOk = false; typeBad.push(a.name + ':missing-in-state.rs'); continue; }
  const fields = parseFields(body);
  const want = fields.map((f) => ({ name: f.name, type: rustToIdl(f.type) }));
  const got = (a.type.fields || []).map((f) => ({ name: f.name, type: f.type }));
  if (want.length !== got.length) { typeOk = false; typeBad.push(`${a.name}:field-count want=${want.length} got=${got.length}`); continue; }
  for (let k = 0; k < want.length; k++) {
    if (want[k].name !== got[k].name || !idlTypeEq(want[k].type, got[k].type)) {
      typeOk = false;
      typeBad.push(`${a.name}.${want[k].name}: want ${JSON.stringify(want[k].type)} got ${JSON.stringify(got[k].type)}`);
    }
  }
}
check('8. account types (IDL vs state.rs)', typeOk, typeBad.join('; ') || `${(idl.accounts || []).length} account types`);

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks passed.`);
if (failed.length) { console.log('FAILED: ' + failed.map((f) => f.name).join(', ')); process.exit(1); }
