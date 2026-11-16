//! Android 平台专属代码：网络适配、存储授权与更新。

#[cfg(target_os = "android")]
pub(crate) mod commands;
#[cfg(target_os = "android")]
pub(crate) mod network;
#[cfg(target_os = "android")]
pub(crate) mod storage;
pub(crate) mod update;
