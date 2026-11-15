//! Persistent access to directories selected through a macOS open panel.
//!
//! A sandboxed macOS app only receives a temporary security scope from the
//! open panel.  Persisting the path alone makes the next automatic receive
//! request ask for access again.  Store a security-scoped bookmark and keep
//! the resolved URL active for the process lifetime instead.

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

/// Create a bookmark while the URL returned by the open panel still has its
/// security scope.  Failure is reported so callers can retain the path as a
/// best-effort fallback on non-sandboxed builds.
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

/// Resolve and activate a previously stored bookmark.  The resolved URL is
/// intentionally leaked after activation: the security scope must remain
/// active while background transfer tasks write files, and the process owns
/// this small object until exit.
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
