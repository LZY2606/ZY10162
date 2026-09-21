//! 领域模型：芯段、约束锦标（marker）与校验规则。

use serde::{Deserialize, Serialize};

/// 一个芯段。`present=false` 表示缺失芯段；缺芯必须给出
/// `gap_min_years > 0`，严禁以“零年”填补。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub id: String,
    pub top: f64,
    pub bottom: f64,
    pub present: bool,
    pub gap_min_years: Option<f64>,
    pub note: String,
    pub version: i64,
}

/// 约束锦标（原始观测的年龄约束）。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Marker {
    pub id: String,
    pub depth: f64,
    /// `layer` 季节层计数 | `volcano` 火山灰锦标 | `isotope` 同位素对齐点 | `anchor` 固定锚点
    pub kind: String,
    /// 硬约束：年龄必须落在 [lo, hi]；软约束：[lo, hi] 仅作为分布支撑
    pub hard: bool,
    pub lo: f64,
    pub hi: f64,
    /// 软分布参数（截断正态）
    pub mean: Option<f64>,
    pub sd: Option<f64>,
    /// 局部分层的多个合理方案（离散年龄层计数方案），非空时在方案间离散抽样
    pub alt_means: Vec<f64>,
    /// 可交换软锦标：同组锦标的年龄归属可以对调（swap）
    pub exchange_group: Option<String>,
    pub weight: f64,
    pub excluded: bool,
    pub note: String,
    pub version: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExchangeState {
    pub group: String,
    pub swapped: bool,
}

/// 一次求解的完整输入（运行快照，不可变）。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SolveInput {
    pub seed: u64,
    pub draws: usize,
    pub solver_version: u32,
    pub segments: Vec<Segment>,
    pub markers: Vec<Marker>,
    pub exchanges: Vec<ExchangeState>,
}

#[derive(Debug)]
pub struct ModelError(pub String);

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Segment {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.id.trim().is_empty() {
            return Err(ModelError("芯段 id 不能为空".into()));
        }
        if !(self.bottom > self.top) {
            return Err(ModelError(format!(
                "芯段 {} 深度必须严格递增（top={}, bottom={}）",
                self.id, self.top, self.bottom
            )));
        }
        if !self.present {
            match self.gap_min_years {
                Some(g) if g > 0.0 => {}
                _ => {
                    return Err(ModelError(format!(
                        "缺芯段 {} 必须给出正的 gap_min_years（不能以零年填补）",
                        self.id
                    )));
                }
            }
        }
        Ok(())
    }
}

impl Marker {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.id.trim().is_empty() {
            return Err(ModelError("锦标 id 不能为空".into()));
        }
        if !matches!(self.kind.as_str(), "layer" | "volcano" | "isotope" | "anchor") {
            return Err(ModelError(format!("锦标 {} 类型非法: {}", self.id, self.kind)));
        }
        if self.hi < self.lo {
            return Err(ModelError(format!("锦标 {} 区间反向 hi<lo", self.id)));
        }
        if self.weight < 0.0 {
            return Err(ModelError(format!("锦标 {} 权重为负", self.id)));
        }
        Ok(())
    }
}

/// 校验全部芯段：严格有序、互不重叠。
pub fn validate_segments(segments: &[Segment]) -> Result<(), ModelError> {
    for s in segments {
        s.validate()?;
    }
    let mut ordered: Vec<&Segment> = segments.iter().collect();
    ordered.sort_by(|a, b| a.top.partial_cmp(&b.top).unwrap());
    for w in ordered.windows(2) {
        // 允许相邻芯段在边界处恰好相接（同一深度面），但不得重叠或逆序。
        if !(w[1].top >= w[0].bottom) {
            return Err(ModelError(format!(
                "芯段深度必须严格有序且不重叠：{}[{},{}] 与 {}[{},{}]",
                w[0].id, w[0].top, w[0].bottom, w[1].id, w[1].top, w[1].bottom
            )));
        }
    }
    Ok(())
}

pub const SOLVER_VERSION: u32 = 3;
