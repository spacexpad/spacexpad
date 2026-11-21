# 设计 (a)：创建代币时遏制建池费不够 —— 最小募资校验

**状态：已按 2026-11-20 用户修正实施（见 §2 修正说明）。目标版本 v4.2.7。** v4.2.6 已被 CPI 修复占用（creatorLp 错位），本设计不进 v4.2.5/v4.2.6。
**日期：2026-11-20。依据：全部实测 / 读码。**

## 1. 问题

发射 A（1000 份 × 0.001 SOL = 募资 1 SOL，LP 份额 60% = 0.6 SOL）永远付不起建池费，
毕业后每次建池都以 `InsufficientLpSol`（6026）失败，crank 无限重试同一干净错误。
问题只能在**创建时**拦住：一个永远建不了池的发射就不该被创建出来。

## 2. 数字（算清）

建池时 handler 的算法（lib.rs `create_raydium_pool_inner`，已读码）：

```
wsol_amount = sol_vault_lamports - (create_pool_fee + RAYDIUM_POOL_RENT_BUFFER)
要求 wsol_amount > 0，否则 InsufficientLpSol（6026）
```

- `create_pool_fee`：链上读 Raydium amm_config（index 0）第 36..44 字节 LE u64。
  - **devnet 实测 1 SOL**（建池交易 `4BZjDk…` 后 fee 账户 G11F… 余额 +1.0 SOL）。
  - **主网实测 0.15 SOL**（2026-11-20 只读查询主网 amm_config `D4FPEruKEHrG5TenZ2mpDGEfu1iUvTiqBxvpU8HLBvC2`，
    字节 36..44 = 150,000,000 lamports；与 Raydium 文档一致，这次是链上核实不是猜）。
- `RAYDIUM_POOL_RENT_BUFFER` = 0.1 SOL（constants.rs；实测建池各账户租金约 0.0528 SOL，0.1 留有余量；
  建池后 sol_vault 实测剩 0.069 SOL，与 0.1 - 租金吻合）。
- LP 份额 = 募资总额 × 60%（`GRAD_LP_SOL_BPS = 6000`）。毕业要求售罄，所以募资总额 = `total_shares × price_per_share`，精确值。

| 集群 | LP 份额需 > | 募资总额需 > |
|---|---|---|
| devnet | 1.1 SOL | **1.8333… SOL** |
| 主网 | 0.25 SOL | **0.4167 SOL** |

**唯一绑定条件是上面的公式**：`total_shares × price_per_share × 60% > createPoolFee + 0.1 SOL`。
不单边限定份数，也不单边限定单价——份数与单价只通过募资总额起作用。
`price_per_share` 本来就是 `create_launch` 的参数（创建者自定），不用加新参数；
现有份数约束（MIN 1,000 / MAX 50,000 / STEP 1,000）不变。

举例（devnet，费 1 SOL + 缓冲 0.1 SOL）：
- 1000 份 @ 0.002 SOL/份 = 募资 2 SOL → LP 份额 1.2 SOL > 1.1 SOL，**通过**；
- 1000 份 @ 0.001 SOL/份 = 募资 1 SOL → LP 份额 0.6 SOL，**不通过**。
初版写的“最小 2000 份”只是 0.001 SOL/份单价下的特例（1834 份按 STEP 取整），不是规则，别写死。

发射 A（1 SOL）不达标，是预期内的反例；发射 B（3 SOL，LP 1.8 ≥ 1.1）达标。
建池费确实从 60% LP 部分出（发射 B 实测：1.8 SOL 的 LP 份额 → 1.0 SOL 费 + 0.1 缓冲 + 0.7 SOL 进池），所以校验盯 LP 份额是对的。

## 3. 校验放在哪

`create_launch` handler（lib.rs），紧贴现有的份数/单价校验之后（`total_shares` 范围 + `price_per_share > 0` 处），
在建任何账户之前拒绝。**必须放在链上程序里**（强制执行），前端/SDK 只做同样的前置提示（用户体验，不作数）。

```rust
// 伪代码（v4.2.7 实现时落到 create_launch 内）
let total_raise = total_shares.checked_mul(price_per_share).ok_or(MathOverflow)?;
let lp_sol = total_raise.checked_mul(GRAD_LP_SOL_BPS).ok_or(MathOverflow)?
    .checked_div(10_000).ok_or(MathOverflow)?;
require!(lp_sol > create_pool_fee + RAYDIUM_POOL_RENT_BUFFER,
    AgentPadError::RaiseTooSmallForPoolCreation);
```

- 用 `>`（严格大于），与建池时 `wsol_amount > 0` 的要求逐字对应：LP 份额刚好等于 费+缓冲 时池里 WSOL 为 0，仍失败。
- 整数除法向下取整对校验方有利（算出的 lp_sol 只会比真实值小，不会放过不达标的）。
- 不改 `MIN_TOTAL_SHARES`（1000）——新校验是叠加的、价格感知的；主网 0.001 SOL/份时现有下限已够，
  但低单价时仍需新校验兜底。

## 4. 费用值从哪来（三种方案，推荐第一种）

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| **(i) 创建时链上读 amm_config（推荐）** | `CreateLaunch` 结构体加一个只读 `amm_config` 账户；handler 验证它是 Raydium PDA（seeds `[b"amm_config", &0u16.to_be_bytes()]`，program = 当集群 CPMM 程序 ID，与 `create_raydium_pool` 里的推导完全一致），读字节 36..44 得 fee | 单一事实源：与建池时读的是同一个值；Raydium 改费自动跟随；无需管理员操作 | CreateLaunch 账户 11 → 12 个；IDL + SDK + 前端都要改（传 amm_config）；每次创建多一次账户校验（可忽略） |
| (ii) 写死常量 | constants.rs 加 `RAYDIUM_CREATE_POOL_FEE_DEVNET = 1 SOL` / `MAINNET = 0.15 SOL`，按集群编译进程序 | 改动最小 | 费用是 Raydium 的、不是我们的；Raydium 一改费就漂移，且 devnet 与主网值不同、单常量必错一边 |
| (iii) GlobalConfig 可配字段 | admin 设 `min_pool_fee`，创建时读它 | 灵活 | 又多一个管理员要维护的值；值 stale 的风险；费用明明在链上有权威源，没必要再抄一份 |

推荐 (i)。`create_raydium_pool` 今天已经是这么读费的（lib.rs 约 1331 行），创建时复用同一读法，
两个时点的 fee 来源完全一致。

## 5. 新错误码

错误枚举当前最大码 **6035**（`InvalidCreatorLpToken`，v4.2.2）。新错误：

```
6036  RaiseTooSmallForPoolCreation
msg: "Total raise is too small: the 60% LP share would not cover the Raydium pool creation fee plus rent buffer."
```

## 6. 附带改动（实施时）

- IDL：`CreateLaunch` 加 `amm_config`（只读）；错误表加 6036。
- SDK（`sdk/agentpad-sdk.mjs`）：`createLaunchIx` 传 amm_config（用 `raydiumPdas` 已有的推导）；
  另给创建表单一个本地预检（同样公式，提前提示最小募资）。
- 前端：创建表单在用户输完份数/单价后即提示“募资 X SOL，LP 份额 Y SOL，建池需 > Z SOL”，不达标禁提交。
- 测试：e2e 加一个“创建 1000 份 @ 0.001（devnet）应被拒绝（6036）”的用例；另加“创建 1000 份 @ 0.002（devnet）应通过”的用例（验证校验看的是募资总额而非份数）；回归：发射 B 参数（3000 份 @ 0.001）仍可创建。
- 版本：**v4.2.7**。不碰已部署的 v4.2.6；不追溯：发射 A 已存在，不受新校验影响（它永远建不了池是已知状态，不再花钱）。

## 7. 不在本设计内

- 不改 fee / buffer 数值；不改 60% 分账；不给发射 A 事后补救（它按设计就是建不了池）。
- 另记一个相关缺口（验证中发现，非本次设计）：`create_raydium_pool` 的结构体约束
  `lp_token_staging: token::authority = launch_pool` 与 `pool_wsol_account: associated_token::authority = launch_pool`
  在首次成功运行后即失效（authority 已改到 sol_vault），重调会在 handler 的幂等 early-return 之前
  被 Anchor 以 `ConstraintAssociated`（2015）挡掉——失败是干净的（原子回滚、状态不动），但“重调即 no-op 成功”
  的理想语义没达成；若未来某次建池在 set_authority 与 Raydium initialize 之间失败，重试会被同一堵墙挡住。
  候选修法（v4.2.7 或以后）：把这两个账户放宽为 UncheckedAccount + handler 内验两种 authority 之一。
