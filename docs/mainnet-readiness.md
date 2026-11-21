# SpaceXpad 主网部署就绪度核查

- 日期：2026-11-20
- 基线：v4.2.7（devnet），待推送到私有仓库的提交 `c9ea777`
- 结论：**不能直接部署到主网。** 合约本身不用改代码（它是跨链设计的），但程序要重新部署到主网、后端改配置、前端有几处写死的 devnet 要修。工作量是“配置 + 小修 + 重新部署”，不是重写。

## 1. 合约：不用改代码，但要重新部署

- **Raydium 程序不是写死的**：`create_raydium_pool` 与 `create_launch` 里的 amm_config 校验都把 Raydium CPMM 程序当账户传进来（`accounts.raydium_program`），amm_config PDA 由该程序地址推导；建池费（createPoolFee）是链上从 amm_config 读的（bytes 36..44），不是写死的。所以同一份 `.so` 在主网也能用。
- **但程序必须重新部署到主网**：当前程序 `DMt9jcZTMgHzzZcEngNj8c1Q6ahVdyLACirZwT71NzA6` 只在 devnet 上。用同一 keypair（`agentpad_phase3/target/deploy/agentpad-v3-keypair.json`）在主网部署会得到同一 programId；换 keypair 则 programId 变了，`shared/constants.json` 要同步改。
- **主网部署后要做的链上初始化**（缺一不可）：
  - `initialize_global`（新建主网 GlobalConfig PDA；admin 与 gate_signer 可复用 devnet 的同一对密钥）。
  - 白名单 PDA：股票资产的 mint 地址本身就是主网地址（NVDAx/TSLAx/AAPLx/MSTRx/ANTx/SPCXx 已在 `constants.json` 里），但白名单 PDA 是按 programId 推导的，主网要重新创建（`whitelist_asset` PDA）。
  - 新平台币 mint：devnet 的 `8SfUM761VMHKfEibY2PDuWMtXGsjUBKd2MbHQxP9rw9B` 只在 devnet 存在，主网要新建一个 mint（平台币经济模型本身仍待用户定）。
- **建池费**：devnet 1 SOL，主网 0.15 SOL（SDK 注释称两者均已链上核实；建池费是链上读的，不用改代码）。最小募资校验（`60% LP > 费 + 0.1 SOL`）在主网变成 `> 0.15 + 0.1 = 0.25 SOL`，即总募资 > 约 0.42 SOL 即可，比 devnet 的 >1.833 SOL 更容易达标。0.1 SOL 租金缓冲（`RAYDIUM_POOL_RENT_BUFFER`）在主网是否仍够用，部署前用主网实测租金复核一次。
- **交易费率**：`raydiumSwapQuote` 的 `tradeFeeRate` 默认值 2500（0.25%，devnet amm_config 费率），但真实兑换路径（`raydiumSwapIxs`）用的是链上读到的 `fees.tradeFeeRate`，所以主网费率不同也不影响；默认值只影响估算兜底。

## 2. 后端：改配置即可，不用改代码

| 项 | 现在（devnet） | 主网改成什么 | 改动点 |
|---|---|---|---|
| cluster | `devnet` | `mainnet` | 设环境变量 `CLUSTER=mainnet`（`server/index.mjs:82`，`process.env.CLUSTER \|\| CONSTANTS.cluster`），或改 `shared/constants.json` 的 `cluster` |
| RPC | `https://api.devnet.solana.com` | 主网 RPC | `constants.json` 里已有 `rpc.mainnet = https://api.mainnet-beta.solana.com`，也可设 `RPC_URL` 环境变量。**强烈建议用专用 RPC（Helius/QuickNode 免费档），不要用公共主网 RPC**——公共主网 RPC 限流比 devnet 严得多，后端每 15 秒轮询 + graduator 订阅 + 每分钟 K 线快照很容易被 429 |
| programId | `DMt9jcZ...`（devnet） | 主网部署后的 programId | `shared/constants.json` 的 `programId`；若用同一 keypair 部署则不变 |
| Raydium 地址 | `constants.json` 的 `raydium.devnet` | `raydium.mainnet` 已有 | `constants.json` 里主网值已备好：CPMM 程序 `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C`、fee 账户 `DNXgeM9EiiaAbaWvwjHj9fQQLAX5ZsfHyvmYUNRAdNC8`、authority `GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL`；后端 `/api/config` 返回 `CONSTANTS.raydium[CLUSTER]`，随 cluster 自动切换 |
| gate keypair | `server/.gate-keypair.json` | 可复用 | 私钥不换；主网 GlobalConfig 初始化时把同一 gate_signer 写进去即可 |
| graduator / candles / poolstats | 用后端 connection | 不用改代码 | 它们拿后端传进去的 connection 与 cluster，随 `CLUSTER`/`RPC_URL` 走 |
| 限流 / CORS | 120 次/分钟，CORS 全开 | 不用改 | 主网同样适用 |
| 平台币 mint | devnet mint | 主网新建 mint | 见 §1；经济模型待用户定 |

## 3. 前端：要修几处写死的 devnet

| 项 | 现在 | 主网要改 | 文件 |
|---|---|---|---|
| cluster 默认值 | `window.AGENTPAD_CLUSTER \|\| 'devnet'`，HTML 没有注入 `AGENTPAD_CLUSTER` | 部署的 HTML 里注入 `window.AGENTPAD_CLUSTER='mainnet'`（与 `AGENTPAD_API_BASE` 同款做法），或把默认值改成从 `/api/config` 读 | `frontend/agentpad-wiring.js:19`、`deploy-*/spacexpad.html` 注入处 |
| 前端连接的 RPC | `conn()` 用 `S.rpcUrl(CLUSTER)`，随 cluster 走 | 不用改（cluster 一改就跟着走） | `frontend/agentpad-wiring.js:181-182` |
| 站内买卖 | `CLUSTER === 'devnet'` 时 Raydium 直连；否则走 Jupiter | **主网路径已写好**（`wiring.js:1664` 起，Jupiter flow，注释写明“kept for later”），cluster 一改就启用；Jupiter 公共 API 是主网 only，主网可用 | `frontend/agentpad-wiring.js:1614,1664` |
| 建池费预估的 RPC | 写死 `Sdk.rpcUrl('devnet')` | 改成 `Sdk.rpcUrl(CLUSTER)`，否则主网预估会去读 devnet 的 amm_config | `frontend/agentpad-wiring.js:1444`（另有 1417/1436 行附近注释提到 devnet 默认 1 SOL，同步改） |
| Phantom Agent Wallet 支持网络 | 写死 `{ devnet: S.rpcUrl('devnet') }` | 改成主网（`mainnet` + 主网 RPC），否则 Phantom MCP 认不到主网 | `frontend/agentpad-wiring.js:1155` |
| 意图提示词 | 文案写“（devnet）”、`networkId: "solana:devnet"`、币安 `--binanceChainId CT_501` | 改成主网文案与 `networkId: "solana:mainnet-beta"`；币安主网 chainId 待核实（CT_501 是 devnet 的，主网值不要猜，部署前查 baw 文档/实测确认） | `frontend/agentpad-wiring.js:1431-1434` 及 skill 同款文案 |
| 页面文案 | “开发网目前只有 SOL”（分红资产提示）、挑战登录说明里的 `networkId 用 solana:devnet` | 改成主网文案；分红资产提示要按主网白名单 PDA 开通情况写 | `frontend/spacexpad.html:320,575` |
| skill 文件 | 全篇 devnet（程序已部署到 devnet、devnet 费 1 SOL、`solana:devnet`、devnet SOL 等） | 出主网版 skill（或把 devnet 引用批量改成主网值与主网程序地址、主网 skill 地址） | `agentpad_phase3/skill/SKILL.md`（改后重新生成部署用的 `skill`/`skill.md`） |
| bundle | 内嵌 `S.CONSTANTS`（cluster、programId、raydium 地址） | `constants.json` 改完后重打 bundle 再部署 | `frontend/agentpad-bundle.js`（`esbuild sdk/browser-entry.mjs`） |

## 4. 不能直接部署的项（直接推主网会失败或出问题）

1. **程序只在 devnet 上**：主网没有这个程序，任何交易都会报“程序不存在”。必须先部署 + 初始化。
2. **前端默认 devnet**：`AGENTPAD_CLUSTER` 没有注入，默认 `'devnet'`，主网部署后前端仍会连 devnet RPC、读 devnet 的池子，页面数据全是 devnet 的。
3. **建池费预估读 devnet**：`wiring.js:1444` 写死 devnet RPC，主网预估会用 devnet 的 1 SOL 费而不是主网的 0.15 SOL，创建表单的“最低募资”提示会算错。
4. **Phantom Agent Wallet 只认 devnet**：`supportedNetworks` 只有 devnet，主网钱包连不上。
5. **白名单 PDA 与平台币 mint 在主网不存在**：分红选股票会报 PDA 不存在；平台币 mint 地址在主网无此账户。
6. **公共主网 RPC**：`api.mainnet-beta.solana.com` 限流严，后端轮询与 graduator 订阅可能被 429，数据变 stale。主网必须配专用 RPC。
7. **意*向提示词与 skill 全是 devnet**：agent 按提示词会在 devnet 上执行，与主网部署不一致。

## 5. 待用户决定的项（附建议）

- [ ] **主网部署用同一 program keypair 还是新 keypair？**
  - **建议：用同一 keypair（programId 不变）。** programId 已写在 `constants.json`、skill、文档与前端 bundle 里，用同一 keypair 只需改 cluster 与 RPC，其他引用都不用动。keypair 本身已妥善保管（600 权限、只在我电脑上），复用不增加实质风险。换新 keypair 则 programId 变了，所有引用都要改一遍，得不偿失。
- [ ] **主网 RPC 用哪家？**
  - **建议：Helius 免费档优先，QuickNode 免费档备选。** 两家都支持 WebSocket 订阅（graduator 要用），都有免费档。关键是不要用公共主网 RPC（`api.mainnet-beta.solana.com`）——它限流严，后端轮询 + 订阅很容易被 429。先用 Helius 免费档起步，量起来了再升级付费档。
- [ ] **平台币 mint 的经济模型（供应/分配）？**
  - **建议：先不急着定。** 平台币不在主网发行的关键路径上（发射流程本身不用它，它的用途还没定）。主网新建 mint 时先保持 0 供应占位（与 devnet 做法一致），等经济模型定了再铸。不要一上来就铸一大笔供应，也不要过早放弃 mint 权限——等模型定了再决定铸多少、怎么分。
- [ ] **币安主网 chainId？**
  - **建议：部署前实测核实，不要猜。** CT_501 是 devnet 的，主网值查 baw 文档（`baw contract-call --help`）或币安 Agentic Wallet 文档确认，确认前不要写死。
- [ ] **是否保留 devnet 站？**
  - **建议：保留。** devnet 站继续做测试环境，主网站单独部署（单独的 Pages 项目、单独的后端配置），两套互不干扰。以后改动先在 devnet 验证再上主网。
- [ ] **真实资金准备。**
  - **建议：部署钱包先准备约 5 SOL 主网 SOL**（程序租金约 3.7 SOL + 建池费 0.15 SOL/池 + 运营预留）。主网第一次先做一个小额测试发射（小份数、低单价），把创建→铸造→毕业→建池→领币全流程跑通一遍，确认没问题再开放。

## 附：改动量级

- 合约：**0 行代码改动**（重新部署 + 链上初始化）。
- 后端：**0 行代码改动**（`CLUSTER`/`RPC_URL` 环境变量 + `constants.json` 的 programId）。
- 前端：**约 6 处小修**（cluster 注入、费预估 RPC、Phantom 支持网络、意图提示词、页面文案、skill）+ 重打 bundle。
- 链上操作：部署程序、初始化 GlobalConfig、建白名单 PDA、建平台币 mint。
