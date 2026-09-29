use hudsucker::certificate_authority::RcgenAuthority;
use hudsucker::rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use hudsucker::rustls::crypto::aws_lc_rs;
use std::path::{Path,PathBuf};
use std::process::Command;

use crate::pause::exit_with;
use crate::platform::notify_proxy_changed;

pub fn proxy_key_path() -> &'static str {
    r"Software\Microsoft\Windows\CurrentVersion\Internet Settings"
}

fn restore_system_proxy(enable: u32, server: &str) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(proxy_key_path())
        .map_err(|e| format!("打开注册表失败：{e}"))?;
    key.set_value("ProxyEnable", &enable).map_err(|e| format!("还原失败：{e}"))?;
    if server.is_empty() {
        let _ = key.delete_value("ProxyServer");
    } else {
        let _ = key.set_value("ProxyServer", &server);
    }
    notify_proxy_changed();
    Ok(())
}

pub fn install_ca(cert: &Path, store: &str) -> Result<(), String> {
    let mut cmd = Command::new("certutil");
    if store == "user" {
        cmd.arg("-user");
    }
    let output = cmd
        .args(["-addstore", "-f", "Root"])
        .arg(cert)
        .output()
        .map_err(|e| format!("调用 certutil 失败：{e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn uninstall_ca(store: &str, common_name: &str) -> Result<(), String> {
    let mut cmd = Command::new("certutil");
    if store == "user" {
        cmd.arg("-user");
    }
    let output = cmd
        .args(["-delstore", "Root", common_name])
        .output()
        .map_err(|e| format!("调用 certutil 失败：{e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// 退出时一定要还原系统代理、卸载证书(Drop 保证所有路径都会执行)
pub struct Guard {
    pub proxy_prev: Option<(u32, String)>,
    pub ca: Option<(String, String)>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some((enable, server)) = self.proxy_prev.take() {
            match restore_system_proxy(enable, &server) {
                Ok(()) => println!("系统代理已还原（原值 {}）", if server.is_empty() { "未启用".into() } else { server }),
                Err(err) => eprintln!("还原系统代理失败：{err}"),
            }
        }
        if let Some((name, store)) = self.ca.take() {
            match uninstall_ca(&store, &name) {
                Ok(()) => println!("根证书已卸载"),
                Err(err) => eprintln!("卸载根证书失败（可手动 certutil -delstore Root \"{name}\"）：{err}"),
            }
        }
    }
}

/// 生成一把自己的 CA 
pub fn creat_user_ca(cert_path: &PathBuf, ca_name: &String) -> RcgenAuthority {
    let ca = {
        let key_pair = match KeyPair::generate() {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("生成密钥失败：{err}");
                exit_with(2);
            }
        };
        let mut params = match CertificateParams::new(Vec::<String>::new()) {
            Ok(params) => params,
            Err(err) => {
                eprintln!("构造证书参数失败：{err}");
                exit_with(2);
            }
        };
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, ca_name.clone());

        let ca_cert = match params.self_signed(&key_pair) {
            Ok(cert) => cert,
            Err(err) => {
                eprintln!("自签名失败：{err}");
                exit_with(2);
            }
        };
        if let Err(err) = std::fs::write(&cert_path, ca_cert.pem()) {
            eprintln!("写证书失败：{err}");
            exit_with(2);
        }
        let issuer = Issuer::new(params, key_pair);
        RcgenAuthority::new(issuer, 1_000, aws_lc_rs::default_provider())
    };
    println!("已生成 CA：{}", cert_path.display());
    ca
}