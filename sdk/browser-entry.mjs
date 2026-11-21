// Browser entry: bundles the SDK + the web3.js pieces the
// frontend wiring needs into one IIFE global `AgentPad`.
//
// agentpad-wiring.js uses bare `Buffer` (base64, tx serialize, ...), so the
// bundle must install it as a browser global — it does not exist in browsers
// (2026-11-19: the page badge failed with "Buffer is not defined" until
// this was added).
import { Buffer } from 'buffer';
globalThis.Buffer = Buffer;

export * from './agentpad-sdk.mjs';
export {
  Connection,
  PublicKey,
  Keypair,
  Transaction,
  VersionedTransaction,
  LAMPORTS_PER_SOL,
  // 2026-11-19 (v=ab): graduate() merges settle + pool creation (+ LP burn
  // in v4.2) into ONE transaction. The merged tx costs ~205-290k CU,
  // above Solana's 200k default per-tx budget, so the wiring prepends a
  // ComputeBudgetProgram.setComputeUnitLimit instruction (600k) — it must
  // come from this bundle, the wiring has no other way to build it.
  ComputeBudgetProgram,
} from '@solana/web3.js';
