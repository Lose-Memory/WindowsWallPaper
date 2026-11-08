//! 引擎错误类型。
//!
//! 不引入 `anyhow`／`thiserror`：本 crate 的错误种类很少，手写足够，
//! 也能让"哪一种失败"在调用处显式可辨。

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// 文件系统操作失败，附带发生位置与底层错误。
    Io {
        context: String,
        source: std::io::Error,
    },
    /// 配置解析失败。
    Config {
        context: String,
        detail: String,
    },
    /// 找不到程序应当存在的某个位置（例如无法确定可执行文件所在目录）。
    NotFound { what: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { context, source } => write!(f, "{context}：{source}"),
            Error::Config { context, detail } => write!(f, "{context}：{detail}"),
            Error::NotFound { what } => write!(f, "找不到{what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl Error {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    pub fn config(context: impl Into<String>, detail: impl Into<String>) -> Self {
        Error::Config {
            context: context.into(),
            detail: detail.into(),
        }
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Error::NotFound { what: what.into() }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
