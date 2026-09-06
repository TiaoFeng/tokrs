//! 自定义AppError枚举
//!
//! 为AppError实现Display和Error trait
//!
use std::{error::Error, fmt, path::Path};

#[derive(Debug)]
pub enum AppError {
    #[allow(dead_code)]
    DoNotFoundAnyFiles,
    Io {
        operation: &'static str,
        path: String,
        source: std::io::Error,
    },
    Corrupted {
        path: String,
        source: serde_json::Error,
    },
    Sqlite {
        path: String,
        source: rusqlite::Error,
    },
}

/// 用于使用端快速的生成 `AppError::Io` 这种错误类型
pub fn io_err(operation: &'static str, path: &Path, err: std::io::Error) -> AppError {
    AppError::Io {
        operation,
        path: path.to_string_lossy().to_string(),
        source: err,
    }
}

/// 用于使用端快速的生成 `AppError::Corrupted` 这种错误类型
pub fn json_err(path: &Path, err: serde_json::Error) -> AppError {
    AppError::Corrupted {
        path: path.to_string_lossy().to_string(),
        source: err,
    }
}

pub fn sqlite_err(path: &Path, err: rusqlite::Error) -> AppError {
    AppError::Sqlite {
        path: path.to_string_lossy().to_string(),
        source: err,
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::DoNotFoundAnyFiles => {
                write!(f, "Do not found any files")
            }
            AppError::Io {
                operation,
                path,
                source,
            } => {
                write!(f, "failed to {} '{}': {}", operation, path, source)
            }
            AppError::Corrupted { path, source } => {
                write!(f, "task file '{}' is corrupted: {}", path, source)
            }
            AppError::Sqlite { path, source } => {
                write!(f, "sqlite error on '{}': {}", path, source)
            }
        }
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            AppError::Io {
                operation: _,
                path: _,
                source,
            } => Some(source),
            AppError::Corrupted { path: _, source } => Some(source),
            AppError::Sqlite { path: _, source } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "tests/error_test.rs"]
mod tests;
