//! prince定价模块
//!
//! 成本解析优先级: 上游自报成本 > ~/.config/tokrs/pricing.json 估价 > 无价(unpriced)
//! pricing.json 支持: 同模型时间版本价(since, 本地日期生效), 长上下文/峰时字段级覆盖块
//! 统计时自动为表中缺失的模型追加全 null 模板(绝不修改已有条目);
//! 文件损坏直接报 Corrupted 退出且不回写, 由用户自行修复
//!
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::{AppError, io_err, json_err};
use crate::io::load;
use crate::model::UsageEntry;
use crate::tokens::local_date;

/// 当前唯一支持的价目表版本
const PRICING_VERSION: u32 = 1;

/// 一组可选价格字段(USD / 1M tokens), null 表示未填
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricingFields {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

impl PricingFields {
    fn is_empty(&self) -> bool {
        self.input.is_none()
            && self.output.is_none()
            && self.cache_read.is_none()
            && self.cache_write.is_none()
    }

    /// 以 self 的非 null 字段覆盖 base(字段级合并)
    fn overlay(&self, base: &PricingFields) -> PricingFields {
        PricingFields {
            input: self.input.or(base.input),
            output: self.output.or(base.output),
            cache_read: self.cache_read.or(base.cache_read),
            cache_write: self.cache_write.or(base.cache_write),
        }
    }
}

/// 峰时定价覆盖块: hours 为 utc_offset 时区的小时区间 [start, end), start>end 跨午夜回绕
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PeakPricing {
    #[serde(default)]
    pub hours: Vec<[u8; 2]>,
    #[serde(default)]
    pub utc_offset: i8,
    #[serde(flatten)]
    pub fields: PricingFields,
}

/// 长上下文定价覆盖块: 请求上下文 token 数 >= above 时生效
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LongContextPricing {
    pub above: u64,
    #[serde(flatten)]
    pub fields: PricingFields,
}

/// 某模型一段时间起生效的价格版本
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PricingVersion {
    /// 生效日期(本地时区, YYYY-MM-DD); 缺省表示始终生效
    #[serde(default)]
    pub since: Option<NaiveDate>,
    #[serde(flatten)]
    pub fields: PricingFields,
    #[serde(default)]
    pub long_context: Option<LongContextPricing>,
    #[serde(default)]
    pub peak: Option<PeakPricing>,
}

/// pricing.json 的完整内容
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricingFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub models: BTreeMap<String, Vec<PricingVersion>>,
}

fn default_version() -> u32 {
    PRICING_VERSION
}

impl Default for PricingFile {
    fn default() -> Self {
        Self {
            version: PRICING_VERSION,
            models: BTreeMap::new(),
        }
    }
}

/// 价目表路径: $XDG_CONFIG_HOME/tokrs/pricing.json, 缺省 ~/.config/tokrs/pricing.json
pub fn pricing_path() -> Result<PathBuf, AppError> {
    let root = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => load::home_dir()?.join(".config"),
    };
    Ok(root.join("tokrs").join("pricing.json"))
}

/// 加载价目表; 文件不存在返回空表; 解析失败返回 Corrupted(调用方据此终止, 不得回写)
pub fn load_pricing(path: &Path) -> Result<PricingFile, AppError> {
    if !path.is_file() {
        return Ok(PricingFile::default());
    }
    let raw = std::fs::read(path).map_err(|e| io_err("read", path, e))?;
    let file: PricingFile = serde_json::from_slice(&raw).map_err(|e| json_err(path, e))?;
    if file.version != PRICING_VERSION {
        let e = std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unsupported pricing version {}", file.version),
        );
        return Err(json_err(path, serde_json::Error::io(e)));
    }
    Ok(file)
}

/// 把 entries 中出现但价目表缺失的模型追加为全 null 模板
///
/// 返回新增数量; 仅当确有新增时才原子回写(临时文件+rename), 已有条目绝不改动
pub fn sync_models(
    table: &mut PricingFile,
    path: &Path,
    entries: &[UsageEntry],
) -> Result<usize, AppError> {
    let models: BTreeSet<&str> = entries.iter().map(|e| e.model.as_str()).collect();
    let mut added = 0usize;
    for model in models {
        if !model.is_empty() && !table.models.contains_key(model) {
            table
                .models
                .insert(model.to_string(), vec![PricingVersion::default()]);
            added += 1;
        }
    }
    if added == 0 {
        return Ok(0);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err("create", dir, e))?;
    }
    let json = serde_json::to_string_pretty(table).map_err(|e| {
        io_err(
            "serialize",
            path,
            std::io::Error::other(format!("pricing table: {e}")),
        )
    })?;
    // 原子替换, 避免中途崩溃留下半截文件
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| io_err("write", &tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| io_err("rename", path, e))?;
    Ok(added)
}

/// 为每条 entry 解析最终成本: 自报无条件优先, 否则查表估价
pub fn resolve(entries: &mut [UsageEntry], table: &PricingFile) {
    for entry in entries {
        entry.cost_usd = entry.self_cost_usd.or_else(|| estimate(entry, table));
    }
}

/// 按定价表估算单条 entry 成本(USD); 无匹配版本或基础价全空返回 None
fn estimate(entry: &UsageEntry, table: &PricingFile) -> Option<f64> {
    let versions = find_versions(table, &entry.model)?;
    let date = local_date(entry.created_at);
    let version = versions
        .iter()
        .filter(|v| v.since.is_none_or(|since| date >= since))
        .max_by_key(|v| v.since.unwrap_or(NaiveDate::MIN))?;
    let mut fields = version.fields.clone();
    // 基础价全 null 视为"用户未填", 不计成本
    if fields.is_empty() {
        return None;
    }
    // 叠加顺序: base -> peak -> long_context, 字段级覆盖
    if let Some(peak) = &version.peak
        && in_peak_hours(peak, entry.created_at)
    {
        fields = peak.fields.overlay(&fields);
    }
    if let Some(long) = &version.long_context {
        let context = entry.input_tokens + entry.cache_read_tokens + entry.cache_creation_tokens;
        if context >= long.above {
            fields = long.fields.overlay(&fields);
        }
    }
    let price = |v: Option<f64>| v.unwrap_or(0.0).max(0.0);
    let cost = entry.input_tokens as f64 * price(fields.input)
        + entry.output_tokens as f64 * price(fields.output)
        + entry.cache_read_tokens as f64 * price(fields.cache_read)
        + entry.cache_creation_tokens as f64 * price(fields.cache_write);
    Some(cost / 1e6)
}

/// 模型名 -> 版本列表: 精确匹配优先, 否则最长前缀匹配(前缀末尾须为非字母数字边界)
///
/// 例: 键 "gpt-5" 可匹配 "gpt-5-codex"/"gpt-5.1-2026" 但不会误配 "gpt-51x"
fn find_versions<'a>(table: &'a PricingFile, model: &str) -> Option<&'a [PricingVersion]> {
    if let Some(v) = table.models.get(model) {
        return Some(v);
    }
    table
        .models
        .iter()
        .filter(|(key, _)| {
            !key.is_empty()
                && model.len() > key.len()
                && model.starts_with(key.as_str())
                && !model.as_bytes()[key.len()].is_ascii_alphanumeric()
        })
        .max_by_key(|(key, _)| key.len())
        .map(|(_, versions)| versions.as_slice())
}

/// 判断 created_at(epoch 秒) 是否落在峰时区间内(任一区间命中即为峰时)
fn in_peak_hours(peak: &PeakPricing, created_at: i64) -> bool {
    let local = created_at + i64::from(peak.utc_offset) * 3600;
    let hour = (local.rem_euclid(86_400) / 3_600) as u8;
    peak.hours.iter().any(|&[start, end]| {
        if start <= end {
            (start..end).contains(&hour)
        } else {
            hour >= start || hour < end
        }
    })
}

#[cfg(test)]
#[path = "tests/prince_test.rs"]
mod tests;
