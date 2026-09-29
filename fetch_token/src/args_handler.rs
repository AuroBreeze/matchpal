use std::path::PathBuf;

pub struct Args {
    pub port: u16,
    pub out: PathBuf,
    pub write_config: PathBuf,
    pub names: Vec<String>,
    pub hosts: Vec<String>,
    pub ca_store: String,
    pub timeout: u64,
    pub keep_going: bool,
    pub keep_ca: bool,
    pub verbose: bool,
    pub log_all: Option<PathBuf>,
    pub no_elevate: bool,
}

const HELP: &str = "\
fetch access_token

  --port <端口>           本机监听端口（默认 8080）
  --out <目录>            工作目录，放 CA 证书（默认 capture）
  --write-config <文件>   命中后写这里（默认 config.local.json）
  --names <列表>          要抓的字段名，逗号分隔
  --hosts <域名列表>      在默认白名单之外再追加域名（逗号分隔）
  --any-host              不限制域名（噪声大：CSRF/资讯流 token 也会被写入）
  --ca-store <machine|user> 证书装机器库（需管理员，默认）还是当前用户库
  --timeout <秒>          最长运行时间（默认 300）
  --keep-going            命中后不退出，继续跑
  --keep-ca               结束后保留根证书（默认卸载）
  --log-all <文件>        把所有经过的请求 URL 记录到该文件
  --verbose               打印所有经过的请求
  --no-elevate            不自动提权（自己保证管理员权限）
  --help                  显示本帮助
";

pub fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut args = Args {
        port: 8080,
        out: PathBuf::from("capture"),
        write_config: PathBuf::from("config.local.json"),
        names: vec![
            "access_token".into(),
            "steam_cn_token".into(),
            "accesstoken".into(),
            "pvp_app_token".into(),
            "token".into(),
        ],
        hosts: default_hosts(),
        ca_store: "machine".into(),
        timeout: 300,
        keep_going: false,
        keep_ca: false,
        verbose: false,
        log_all: None,
        no_elevate: false,
    };
    let mut index = 0;
    let value = |i: usize| argv.get(i + 1).cloned().unwrap_or_default();
    while index < argv.len() {
        match argv[index].as_str() {
            "--port" => {
                args.port = value(index).parse().unwrap_or(8080);
                index += 2;
            }
            "--out" => {
                args.out = PathBuf::from(value(index));
                index += 2;
            }
            "--write-config" => {
                args.write_config = PathBuf::from(value(index));
                index += 2;
            }
            "--names" => {
                args.names = value(index).split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
                index += 2;
            }
            "--ca-store" => {
                args.ca_store = value(index);
                index += 2;
            }
            "--hosts" => {
                // 追加而不是替换：漏抓时用户往往只想补一个域名
                // 替换语义会逼他把默认 11 个再抄一遍，抄漏了就是新的漏抓
                args.hosts.extend(normalize_hosts(&value(index)));
                index += 2;
            }
            "--any-host" => {
                args.hosts.clear();
                index += 1;
            }
            "--timeout" => {
                args.timeout = value(index).parse().unwrap_or(300);
                index += 2;
            }
            "--log-all" => {
                args.log_all = Some(PathBuf::from(value(index)));
                index += 2;
            }
            "--keep-going" => {
                args.keep_going = true;
                index += 1;
            }
            "--keep-ca" => {
                args.keep_ca = true;
                index += 1;
            }
            "--verbose" => {
                args.verbose = true;
                index += 1;
            }
            "--no-elevate" => {
                args.no_elevate = true;
                index += 1;
            }
            "--help" | "-h" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            other => {
                eprintln!("未知参数：{other}（用 --help 看用法）");
                std::process::exit(2);
            }
        }
    }
    args
}

// ---------------------------------------------------------------- 主机白名单
/// 默认只认这些域名：完美世界竞技平台（wmpvp）+ Steam 系
///
/// `wmpvp.com` / `pwesports.cn` 是**实测的命中域名**（见仓库根 README 的抓取记录）：
/// `pwaweblogin.wmpvp.com` 下发/携带 `steam_cn_token`、`appactivity.wmpvp.com` 是对战接口、
/// `gwapi.pwesports.cn` 把 token 挂在 URL 上。少了它们就是完全抓不到
///
/// 另外这道闸也是防误收的：`assets.msn.cn`（Windows 小组件资讯流）会送来一个叫
/// `userauthtoken` 的 JWT，落盘后还会被别名逻辑写成 `access_token`，下游拿着它请求必然失败
pub fn default_hosts() -> Vec<String> {
    [
        "wmpvp.com",
        "pwesports.cn",
        "wanmei.com",
        "wanmei.com.cn",
        "wanmei.net",
        "perfectworld.com",
        "perfectworld.com.cn",
        "perfectworld.net",
        "pwrd.com",
        "steampowered.com",
        "steamcommunity.com",
        "steamgames.com",
        "steamstatic.com",
        "steamchina.com",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

pub fn normalize_hosts(list: &str) -> Vec<String> {
    list.split(',')
        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}