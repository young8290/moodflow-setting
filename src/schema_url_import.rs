//! 协议 `moodflow://import/schema` 的方案包下载：仅官方域名（每一跳）+ 限额下载到私有临时文件。
//!
//! 下载机制照抄 `theme_import`（ureq 同步请求，超时 + 体积上限），但方案包是
//! 二进制 zip，限额放宽到 64 MB / 30s。下载成功后由 main 的协议路由转交
//! `scheme.previewImport` + schema_manager 既有确认对话框，本模块不做导入语义。
//!
//! 重定向不交给 ureq 自动跟：它会跨域、也会 https→http 降级，白名单只拦得住第一跳。
//! 这里关掉自动跟随，每一跳都重新过 [`check_official_url`]，读正文前最终地址必在官方域。

use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// 方案包体积上限（zip 内含词库，比主题大得多）。
const MAX_SCHEMA_BYTES: usize = 64 * 1024 * 1024;
/// 下载超时。
const DOWNLOAD_TIMEOUT_SECS: u64 = 30;
/// 最多跟随的重定向跳数（与原先交给 ureq 时的 `.redirects(3)` 同额）。
const MAX_REDIRECTS: usize = 3;
/// 下载落盘文件名前缀；清理陈旧残留只认这个前缀。
const FILE_PREFIX: &str = "moodflow_schema_import-";
/// 早于这个时长的残留（进程在确认框开着时被杀）下次下载时顺手删掉。
const STALE_SECS: u64 = 24 * 60 * 60;

/// 第一跳只接受 Moodflow 仓库；GitHub 的发布资产跳转单独校验。
pub fn is_official_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com")
        || host.eq_ignore_ascii_case("raw.githubusercontent.com")
}

pub fn https_host_of(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    url.host_str().map(str::to_ascii_lowercase)
}

fn repo_url(value: &str) -> bool {
    let Some(host) = https_host_of(value) else {
        return false;
    };
    if !is_official_host(&host) {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    let path = url.path();
    path.starts_with("/young8290/Moodflow/") && !path.contains('%') && !path.contains('\\')
}

fn asset_url(value: &str) -> bool {
    matches!(
        https_host_of(value).as_deref(),
        Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
    )
}

/// 一次 HTTP 往返的结果：重定向交回调用方逐跳校验，正文已按上限读完。
pub(crate) enum Fetched {
    Redirect(String),
    Body(Vec<u8>),
}

/// 第一跳的校验：https + 官方域。
fn check_official_url(url: &str) -> Result<(), String> {
    if !repo_url(url) {
        return Err("方案包链接必须是 Moodflow 自有 GitHub 仓库的 HTTPS 地址".into());
    }
    Ok(())
}

/// 把 `Location` 解析成绝对地址：绝对 URL 原样、`//host/…` 补 https、`/path` 拼回当前
/// 源站。相对路径（`a/b.zip`）官方源站用不到，不解析、按无效处理。
fn resolve_location(current: &str, location: &str) -> Option<String> {
    let loc = location.trim();
    let scheme_len = loc.find("://").unwrap_or(0);
    if scheme_len > 0 && loc[..scheme_len].bytes().all(|b| b.is_ascii_alphabetic()) {
        return Some(loc.to_string());
    }
    if let Some(rest) = loc.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    if loc.starts_with('/') {
        let authority = current.get(8..)?.split(['/', '?', '#']).next()?;
        return Some(format!("https://{authority}{loc}"));
    }
    None
}

/// 按「每一跳先校验、再请求」的次序取方案包正文。`fetch` 只做一次往返、不跟随重定向。
///
/// 拆出 `fetch` 是为了单测：真 TLS 起不了本地服务，注入假往返就能覆盖跨域与降级重定向。
pub(crate) fn fetch_official(
    url: &str,
    mut fetch: impl FnMut(&str) -> Result<Fetched, String>,
) -> Result<Vec<u8>, String> {
    check_official_url(url)?;
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        match fetch(&current)? {
            Fetched::Body(bytes) => return Ok(bytes),
            Fetched::Redirect(location) => {
                let next = resolve_location(&current, &location)
                    .filter(|u| repo_url(u) || asset_url(u))
                    .ok_or_else(|| {
                        "方案包链接重定向到了非官方或非 HTTPS 地址，已拒绝".to_string()
                    })?;
                current = next;
            }
        }
    }
    Err(format!("方案包链接重定向超过 {MAX_REDIRECTS} 次，已放弃"))
}

/// 真实的一次往返：不自动跟随重定向，且只走 https（`https_only` 是兜底，逐跳校验已拦）。
fn http_fetch(agent: &ureq::Agent, url: &str) -> Result<Fetched, String> {
    let response = agent
        .get(url)
        .call()
        .map_err(|e| format!("下载方案包失败：{e}"))?;
    if (300..400).contains(&response.status()) {
        return response
            .header("location")
            .map(|l| Fetched::Redirect(l.to_string()))
            .ok_or_else(|| format!("下载方案包失败：{} 重定向缺少地址", response.status()));
    }
    let mut reader = response.into_reader().take((MAX_SCHEMA_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取方案包响应失败：{e}"))?;
    if bytes.len() > MAX_SCHEMA_BYTES {
        return Err("方案包超过 64 MB 限制".to_string());
    }
    Ok(Fetched::Body(bytes))
}

/// 下载落盘的方案包。**drop 即删文件**：确认对话框是异步的，文件要活到用户确认或取消
/// （见 schema_manager 的 `PendingImport::Downloaded`），之后随待导入项一起释放。
#[derive(Debug)]
pub struct DownloadedPackage {
    path: PathBuf,
}

impl DownloadedPackage {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DownloadedPackage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 下载落盘的目录。
///
/// Linux：`$XDG_CACHE_HOME/Moodflow/import`（0700）。`/tmp` 多用户共享，固定文件名会被
/// 别的用户先占住（确定性拒绝服务），没开 `fs.protected_regular` 时还能在确认前换包。
/// Windows / macOS：系统临时目录本就是每用户私有（`%LOCALAPPDATA%\Temp`、`$TMPDIR`），
/// 沿用；随机文件名与用后即删照样适用。
fn download_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let dir = dirs::cache_dir()
            .ok_or_else(|| "无法确定用户缓存目录（HOME 未设置）".to_string())?
            .join("Moodflow")
            .join("import");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("创建下载目录 {} 失败：{e}", dir.display()))?;
        // 目录早已存在（旧版本或用户自建）时 mode 不生效，补一次。
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("设置下载目录权限失败：{e}"))?;
        Ok(dir)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(std::env::temp_dir())
    }
}

/// 删掉 `dir` 里本模块留下的陈旧残留（只认 [`FILE_PREFIX`]、只删早于 [`STALE_SECS`] 的）。
/// 刚下的、确认框还开着的不碰。
fn purge_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let max_age = std::time::Duration::from_secs(STALE_SECS);
    for e in entries.flatten() {
        let stale = e.file_name().to_string_lossy().starts_with(FILE_PREFIX)
            && e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > max_age));
        if stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 随机文件名 + `create_new`（O_EXCL）落盘：撞名重试，绝不打开别人预先放好的文件或链接。
pub(crate) fn save_package(dir: &Path, bytes: &[u8]) -> Result<DownloadedPackage, String> {
    for _ in 0..16 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
        );
        let path = dir.join(format!("{FILE_PREFIX}{:016x}.zip", h.finish()));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut file = match opts.open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("写入临时文件失败：{e}")),
        };
        // 先交给守卫：写一半失败也随 drop 删掉。
        let pkg = DownloadedPackage { path };
        file.write_all(bytes)
            .map_err(|e| format!("写入临时文件失败：{e}"))?;
        return Ok(pkg);
    }
    Err("写入临时文件失败：随机文件名连续冲突".to_string())
}

/// 生产用 agent：不自动跟随重定向（交给 [`fetch_official`] 逐跳校验），只走 https。
fn download_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .redirects(0)
        .https_only(true)
        .build()
}

/// 校验官方域（每一跳）并下载方案包到私有临时文件，返回 drop 即删的句柄。
pub fn download_schema_package(url: &str) -> Result<DownloadedPackage, String> {
    let agent = download_agent();
    let bytes = fetch_official(url, |u| http_fetch(&agent, u))?;
    let dir = download_dir()?;
    purge_stale(&dir);
    save_package(&dir, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_urls_are_scoped_to_our_repository() {
        assert!(repo_url(
            "https://github.com/young8290/Moodflow/releases/download/v1/a.wpkg"
        ));
        assert!(repo_url(
            "https://raw.githubusercontent.com/young8290/Moodflow/main/a.wpkg"
        ));
        for url in [
            "https://github.com/other/Moodflow/a.wpkg",
            "https://github.com/young8290/Moodflow.evil/a.wpkg",
            "https://github.com/young8290/Moodflow/../other/a.wpkg",
            "https://github.com/young8290/Moodflow/%2Fother",
            "https://user@github.com/young8290/Moodflow/a.wpkg",
            "https://github.com:8443/young8290/Moodflow/a.wpkg",
            "https://github.com.evil/young8290/Moodflow/a.wpkg",
            "http://github.com/young8290/Moodflow/a.wpkg",
            "https://release-assets.githubusercontent.com/a.wpkg",
            "https:/汉字",
            "汉https://x",
            "https://",
        ] {
            assert!(!repo_url(url), "{url}");
        }
    }

    /// 假往返：按地址查表应答，记下实际请求过的地址。
    fn scripted<'a>(
        table: &'a [(&'a str, Result<&'a str, &'a str>)],
        seen: &'a std::cell::RefCell<Vec<String>>,
    ) -> impl FnMut(&str) -> Result<Fetched, String> + 'a {
        move |u| {
            seen.borrow_mut().push(u.to_string());
            match table.iter().find(|(k, _)| *k == u) {
                Some((_, Ok(body))) => Ok(Fetched::Body(body.as_bytes().to_vec())),
                Some((_, Err(loc))) => Ok(Fetched::Redirect(loc.to_string())),
                None => Err(format!("unexpected request {u}")),
            }
        }
    }

    /// 官方域直链与官方域内的重定向（含 `/path` 与 `//host` 写法）照常下载。
    #[test]
    fn fetch_follows_official_redirects() {
        let seen = Default::default();
        let table = [("https://github.com/young8290/Moodflow/a.zip", Ok("PK"))];
        let body = fetch_official(
            "https://github.com/young8290/Moodflow/a.zip",
            scripted(&table, &seen),
        );
        assert_eq!(body.unwrap(), b"PK");

        let seen = Default::default();
        let table = [
            (
                "https://github.com/young8290/Moodflow/s/a",
                Err("/young8290/Moodflow/files/a.zip"),
            ),
            (
                "https://github.com/young8290/Moodflow/files/a.zip",
                Err("https://release-assets.githubusercontent.com/a.zip?token=test"),
            ),
            (
                "https://release-assets.githubusercontent.com/a.zip?token=test",
                Err("https://objects.githubusercontent.com/a.zip"),
            ),
            ("https://objects.githubusercontent.com/a.zip", Ok("PK")),
        ];
        let body = fetch_official(
            "https://github.com/young8290/Moodflow/s/a",
            scripted(&table, &seen),
        );
        assert_eq!(body.unwrap(), b"PK");
        assert_eq!(seen.borrow().len(), 4);
    }

    /// 跨域与 https→http 降级的重定向必须拒绝，且**不向那个地址发请求**——
    /// 交给 ureq 自动跟随时，白名单只校验了第一跳。
    #[test]
    fn fetch_rejects_foreign_or_downgraded_redirects() {
        for bad in [
            "https://evil.com/a.zip",
            "https://evil-moodflow.com/a.zip",
            "http://moodflow.com/a.zip",
            "//evil.com/a.zip",
            "https://github.com/young8290/Moodflow@evil.com/a.zip",
            "ftp://moodflow.com/a.zip",
            "a.zip",
        ] {
            let seen = std::cell::RefCell::new(Vec::new());
            let table = [("https://github.com/young8290/Moodflow/a", Err(bad))];
            let err = fetch_official(
                "https://github.com/young8290/Moodflow/a",
                scripted(&table, &seen),
            )
            .unwrap_err();
            assert!(err.contains("已拒绝"), "{bad}: {err}");
            assert_eq!(seen.borrow().len(), 1, "{bad}: 不得请求重定向目标");
        }
    }

    #[test]
    fn fetch_gives_up_after_max_redirects() {
        let seen = Default::default();
        let table = [(
            "https://github.com/young8290/Moodflow/loop",
            Err("/young8290/Moodflow/loop"),
        )];
        let err = fetch_official(
            "https://github.com/young8290/Moodflow/loop",
            scripted(&table, &seen),
        )
        .unwrap_err();
        assert!(err.contains("超过"), "{err}");
        assert_eq!(seen.borrow().len(), MAX_REDIRECTS + 1);
    }

    /// 真 ureq 的往返语义：`redirects(0)` 时 3xx 原样交回（不报错、不跟随），正文照读；
    /// `https_only` 的 agent 对 http 地址直接拒绝。本地起不了 TLS，这里用明文本地服务
    /// 验前两条，生产 agent 的 https_only 另验第三条。
    #[test]
    fn http_fetch_hands_back_redirects_without_following() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let worker = std::thread::spawn(move || {
            for _ in 0..2 {
                let req = server.recv().unwrap();
                let resp = if req.url() == "/r" {
                    tiny_http::Response::from_string("")
                        .with_status_code(302)
                        .with_header(
                            tiny_http::Header::from_bytes("Location", "https://evil.com/x")
                                .unwrap(),
                        )
                } else {
                    tiny_http::Response::from_string("PK")
                };
                req.respond(resp).unwrap();
            }
        });
        let agent = ureq::AgentBuilder::new().redirects(0).build();
        match http_fetch(&agent, &format!("http://127.0.0.1:{port}/r")).unwrap() {
            Fetched::Redirect(l) => assert_eq!(l, "https://evil.com/x"),
            Fetched::Body(_) => panic!("不应跟随重定向"),
        }
        match http_fetch(&agent, &format!("http://127.0.0.1:{port}/a.zip")).unwrap() {
            Fetched::Body(b) => assert_eq!(b, b"PK"),
            Fetched::Redirect(_) => panic!("应读到正文"),
        }
        worker.join().unwrap();

        assert!(http_fetch(&download_agent(), &format!("http://127.0.0.1:{port}/a.zip")).is_err());
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wind_setting_schema_dl_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 随机名 + O_EXCL：两次落盘不同名、互不覆盖；drop 即删。
    #[test]
    fn save_package_uses_fresh_names_and_drop_deletes() {
        let dir = scratch_dir("save");
        let a = save_package(&dir, b"one").unwrap();
        let b = save_package(&dir, b"two").unwrap();
        assert_ne!(a.path(), b.path());
        assert!(
            a.path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(FILE_PREFIX)
        );
        assert_eq!(std::fs::read(a.path()).unwrap(), b"one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(a.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let (pa, pb) = (a.path().to_path_buf(), b.path().to_path_buf());
        drop(a);
        assert!(!pa.exists());
        assert!(pb.exists());
        drop(b);
        assert!(!pb.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 陈旧残留只删本模块前缀的；别的文件不碰。
    #[test]
    fn purge_stale_only_touches_our_old_files() {
        let dir = scratch_dir("purge");
        let ours = dir.join(format!("{FILE_PREFIX}old.zip"));
        let other = dir.join("someone_else.zip");
        let fresh = save_package(&dir, b"x").unwrap();
        for p in [&ours, &other] {
            let f = std::fs::File::create(p).unwrap();
            f.set_modified(
                std::time::SystemTime::now() - std::time::Duration::from_secs(STALE_SECS + 60),
            )
            .unwrap();
        }
        purge_stale(&dir);
        assert!(!ours.exists());
        assert!(other.exists());
        assert!(fresh.path().exists(), "刚下的不删");
        drop(fresh);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 白名单校验发生在任何网络请求之前——非官方域直接拒绝，不发请求。
    #[test]
    fn download_rejects_non_official_before_request() {
        let err = download_schema_package("https://evil-moodflow.com/pkg.zip").unwrap_err();
        assert!(err.contains("自有 GitHub"), "{err}");
        let err = download_schema_package("http://moodflow.com/pkg.zip").unwrap_err();
        assert!(err.contains("HTTPS"), "{err}");
    }
}
