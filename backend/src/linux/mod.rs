//! Linux 平台专属代码。

#[cfg(target_os = "linux")]
pub(crate) mod autostart;
#[cfg(target_os = "linux")]
pub(crate) mod fullscreen;
pub(crate) mod update;
