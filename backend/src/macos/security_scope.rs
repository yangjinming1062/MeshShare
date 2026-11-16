//! 通过 macOS 打开面板选择目录后的持久访问。
//!
//! 沙盒化的 macOS 应用只能从打开面板获得临时的安全作用域；只保存路径会让下一次自动接收
//! 再次请求授权。这里保存 security-scoped bookmark，并在进程存活期间保持解析出的 URL
//! 处于激活状态。

use anyhow::Context;
use core_foundation::base::TCFType;
use core_foundation::data::CFData;
use core_foundation::url::CFURL;
use core_foundation_sys::base::kCFAllocatorDefault;
use core_foundation_sys::url::{
    kCFURLBookmarkCreationWithSecurityScope, kCFURLBookmarkResolutionWithSecurityScope,
    CFURLCreateBookmarkData, CFURLCreateByResolvingBookmarkData,
    CFURLStartAccessingSecurityScopedResource,
};
use std::path::{Path, PathBuf};
use std::ptr;

/// 在打开面板返回的 URL 仍持有安全作用域时创建 bookmark。
/// 失败会返回错误，调用方在非沙盒构建里可以退化为只保留路径。
pub fn create_bookmark(path: &Path) -> anyhow::Result<Vec<u8>> {
    let url = CFURL::from_path(path, true).context("无法创建接收目录 URL")?;
    let mut error = ptr::null_mut();
    let data_ref = unsafe {
        CFURLCreateBookmarkData(
            kCFAllocatorDefault,
            url.as_concrete_TypeRef(),
            kCFURLBookmarkCreationWithSecurityScope,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut error,
        )
    };
    if data_ref.is_null() {
        anyhow::bail!("无法保存接收目录授权");
    }
    let data = unsafe { CFData::wrap_under_create_rule(data_ref) };
    Ok(data.bytes().to_vec())
}

/// 解析并激活先前保存的 bookmark。解析出的 URL 在激活后被有意泄漏：
/// 后台传输任务写入文件期间安全作用域必须保持有效，进程退出时才释放这个对象。
pub fn activate_bookmark(bookmark: &[u8]) -> anyhow::Result<PathBuf> {
    if bookmark.is_empty() {
        anyhow::bail!("接收目录授权为空");
    }
    let data = CFData::from_buffer(bookmark);
    let mut stale = 0;
    let mut error = ptr::null_mut();
    let url_ref = unsafe {
        CFURLCreateByResolvingBookmarkData(
            kCFAllocatorDefault,
            data.as_concrete_TypeRef(),
            kCFURLBookmarkResolutionWithSecurityScope,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut stale,
            &mut error,
        )
    };
    if url_ref.is_null() {
        anyhow::bail!("无法恢复接收目录授权");
    }
    let url = unsafe { CFURL::wrap_under_create_rule(url_ref) };
    let path = url.to_path().context("无法读取接收目录授权路径")?;
    let started = unsafe { CFURLStartAccessingSecurityScopedResource(url.as_concrete_TypeRef()) };
    if started == 0 {
        anyhow::bail!("macOS 未授予接收目录访问权限");
    }
    std::mem::forget(url);
    Ok(path)
}
