use crate::autostart::{atomic_write, home_directory, remove_optional, wrapper_script};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const BUNDLE_IDENTIFIER: &str = "com.meshshare.desktop";

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn launch_agent_file() -> Result<PathBuf> {
    Ok(home_directory()?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{BUNDLE_IDENTIFIER}.plist")))
}

fn wrapper_file() -> Result<PathBuf> {
    Ok(home_directory()?
        .join("Library")
        .join("Application Support")
        .join("MeshShare")
        .join("autostart.sh"))
}

pub(crate) fn enable(executable: &Path) -> Result<()> {
    let path = launch_agent_file()?;
    let wrapper = wrapper_file()?;
    atomic_write(&wrapper, &wrapper_script(executable, &path))?;
    let shell = xml_escape(
        Path::new("/bin/sh")
            .to_str()
            .context("当前程序路径不是有效的 UTF-8")?,
    );
    let wrapper = xml_escape(wrapper.to_str().context("自启动脚本路径不是有效的 UTF-8")?);
    let contents = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{BUNDLE_IDENTIFIER}</string><key>AssociatedBundleIdentifiers</key><array><string>{BUNDLE_IDENTIFIER}</string></array><key>ProgramArguments</key><array><string>{shell}</string><string>{wrapper}</string></array><key>RunAtLoad</key><true/><key>LimitLoadToSessionType</key><string>Aqua</string></dict></plist>\n",
    );
    atomic_write(&path, &contents)
}

pub(crate) fn disable() -> Result<()> {
    let path = launch_agent_file()?;
    let wrapper = wrapper_file()?;
    remove_optional(path)?;
    remove_optional(wrapper)
}

pub(crate) fn is_enabled() -> Result<bool> {
    Ok(launch_agent_file()?.exists() && wrapper_file()?.exists())
}

#[cfg(test)]
mod tests {
    use super::xml_escape;

    #[test]
    fn plist_values_are_xml_escaped() {
        assert_eq!(xml_escape("a<&\"'"), "a&lt;&amp;&quot;&apos;");
    }
}
