# 寒层年尺（hanleng-nianchi）

本地冰芯深度 → 年代建模服务。基于季节层计数、火山灰锦标、同位素对齐点、
硬年代区间与缺芯记录，构建**深度严格有序、年龄只增不减**的单调年代曲线；
不依赖任何外部数据服务，全部计算在本机完成。

技术栈：Rust + Axum + SQLite（rusqlite/bundled）+ 无框架 Web UI。

## 安装与演示

```bash
cargo fetch --locked
cargo test --locked
cargo run --locked -- --listen 127.0.0.1:5502
```

浏览器打开 <http://127.0.0.1:5502>，页面顶部应显示 **寒层年尺**。

其它参数：`--db PATH` 指定 SQLite 文件（默认 `data/hanleng-nianchi.sqlite3`），
`--reset` 清空并重新写入固定 fixture。数据库不存在或为空时自动初始化 fixture。

## 数据口径

- 深度单位为米，年龄单位为“距基准年（BP 风格）年数”，深度越大年龄越大。
- `layer` 季节层计数：`mean` 为从 `start_depth` 到证据深度累计的层数（年），
  按实芯厚度分配到相邻边缘的年增量先验；缺芯区间不参与层数分摊。
- `ash` 火山灰、`isotope` 同位素：软锦标/软对齐点，`mean ± sigma`（年）。
  同一深度多个软约束按精度 1/σ² 加权合并；可编辑分布、暂时排除或交换。
- `hard` 硬年代区间：闭区间 `[age_lo, age_hi]`，同深度取所有硬区间的交集。
  两个区间**刚好相触（端点相等）视为可行**；无交集则拒绝发布。
- `gap` 缺芯：`[depth, gap_end)` 为缺失段，不能用零年填补。缺芯持续时间
  服从以 `gap_mean/gap_sigma` 参数化的正 Gamma，下限为 `max(mean-2σ, 1)` 年；
  证据不允许落在缺芯内部。
- 表面（深度 0）年龄固定为 `surface_age`（默认 0）。

## 模型与重放

- 硬约束先做前/后向传播，得到各深度的可行包络；不可行时拒绝发布。
- 锚点（硬区间、软点）按深度顺序抽样：硬区间在可行盒内均匀抽样，软点在
  可行盒内截断正态抽样。锚点之间，缺芯时长先抽正 Gamma，剩余年预算按
  层数加权的 Dirichlet 分配给实芯边缘，天然保证单调不减；首锚点之上、
  末锚点之下为开区间独立正增量。
- RNG 为内建 xoshiro256\*\*（无外部依赖），每条随机流由“固定种子 +
  物理稳定标签（节点 id / 边缘起点 / draw 序号）”派生。
- `run_id = SHA256(规范化输入)`，运行记录一经写入不可变；同一种子、同一
  输入重放得到**完全相同的分位数**（线性插值分位数）。

### 最小冲突集

硬约束不可行时，求解器以表面固定年龄与缺芯最小时长为永久前提，对可移除
硬区间做迭代删除，返回**包含最小（inclusion-minimal）的冲突集**，并在
记录中给出冲突深度、传播后的上下界与原因（同深度无交集 / 单调性冲突）。

### 缺芯边界调整的局部失效

调整缺芯边界后，只令“受影响深度跨度内、所在锚点跨段中的非锚点节点”的
缓存分位数失效；锚点年龄与不受影响段的分位数逐位保留。接口返回被失效的
节点键列表（页面会显示）。注意：用于重放的**不可变运行记录不会被改写**，
缓存失效只影响当前分位数缓存；重新求解会生成一条新记录。

## 页面与 API

页面支持：查看证据、暂时排除/恢复、交换前两个软锦标、编辑软锦标分布、
调整缺芯边界、求解、查看年龄中位数/区间/局部累积率/软约束冲突、对比两条
运行并定位差异深度段、导出整库、导入复核、清空重置。

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/workspace` | 证据、设置、当前分位数缓存 |
| POST | `/api/evidence` | 新增/更新证据（JSON） |
| POST | `/api/evidence/{id}/enabled` | 暂时排除或恢复 |
| POST | `/api/evidence/{id}/distribution` | 调整软锦标 mean/sigma |
| POST | `/api/evidence/{id}/gap` | 调整缺芯边界，返回失效节点 |
| POST | `/api/soft/swap` | 交换前两个软锦标分布 |
| POST | `/api/settings` | 更新种子、抽样数、分位点等 |
| POST | `/api/solve` | 求解并写入不可变运行记录 |
| GET | `/api/runs`、`/api/runs/{id}` | 列出/查看运行 |
| GET | `/api/runs/{id}/export` | 导出单条运行 JSON |
| POST | `/api/runs/compare` | 两运行差异按深度段输出 |
| GET | `/api/export` / POST | `/api/import` | 整库 bundle 导出 / replace、merge 导入 |
| POST | `/api/reset` | 清空并重置固定 fixture |

## 清空后重新导入复核

1. 页面“导出整库”（或 `GET /api/export`）得到 bundle JSON。
2. 点“清空并重置 fixture”，或删除 SQLite 文件后重启（会写入默认 fixture）；
   也可用 `--reset`。
3. 在导入区选择 bundle，选 `replace`（先清空）或 `merge`（按 run_id 去重）。
4. 复核方式：导入后重新求解，比较新记录 `run_id` 与分位数是否与导入的
   历史记录一致；相同输入 + 相同种子必须逐位一致。bundle 内含
   `settings/evidence/runs/quantiles`，schema 为 `hanleng-nianchi/bundle/v1`。

## 固定 fixture（验收）

`src/fixture.rs` 内置：

- 两个可交换火山灰软锦标 `ash-1`(20 m) 与 `ash-2`(35 m)；
- 同位素对齐点 `isotope-1`(90 m)、季节层计数 `layer-A`、`layer-C`；
- 两个可行硬区间 `hard-A`(50 m,[300,340])、`hard-C`(100 m,[630,690])；
- 缺芯 `gap-BC`(70–82.6 m，正持续时间)；
- 默认停用的冲突硬区间 `hard-X`(50 m,[346,380])：在页面启用后再求解，
  系统返回最小冲突集 `{hard-A, hard-X}`，与 `hard-A` 相触版本（下界 340）
  则可行。

## 测试

`tests/acceptance.rs` 覆盖单调性、固定种子逐位重放、软锦标可交换/排除、
硬冲突与最小冲突集、相触可行、缺芯正时、缺芯移动的精确局部失效、非法几何、
SQLite 清空导入复核；`tests/http_flow.rs` 为真实端口的 HTTP 端到端测试。
