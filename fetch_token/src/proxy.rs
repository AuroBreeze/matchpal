use crate::{ca::proxy_key_path, platform::notify_proxy_changed};

/// 返回 (原 ProxyEnable, 原 ProxyServer)
pub fn set_system_proxy(port: u16) -> Result<(u32, String), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    // winreg 0.56 起 create_subkey 返回 (RegKey, RegDisposition) 元组，必须解构
    let (key, _) = hkcu
        .create_subkey(proxy_key_path())
        .map_err(|e| format!("打开注册表失败：{e}"))?;
    let prev_enable: u32 = key.get_value("ProxyEnable").unwrap_or(0);
    let prev_server: String = key.get_value("ProxyServer").unwrap_or_default();
    key.set_value("ProxyEnable", &1u32).map_err(|e| format!("写 ProxyEnable 失败：{e}"))?;
    key.set_value("ProxyServer", &format!("127.0.0.1:{port}"))
        .map_err(|e| format!("写 ProxyServer 失败：{e}"))?;
    notify_proxy_changed();
    Ok((prev_enable, prev_server))
}