//! 应用内升级(#129)的取数一侧:问 GitHub 最新版、挑出 APK、下载并按 sha256 核对。
//!
//! 装是平台的事(安卓走 PackageInstaller,见 `apps/android`),这一层只交出一个
//! 核对过的文件。资产名与校验文件的格式由 `release/README.md`「APK 资产约定」定。
//!
//! 这几条请求**不带登录态**:目标是 GitHub,把我们的 token 发过去就是泄露。

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{ApiError, platform};

/// 最新的那个正式 release。GitHub 的 latest 本来就不含预发布。
const LATEST: &str = "https://api.github.com/repos/yebei199/osmosis/releases/latest";

/// 本构建的版本号,即 workspace 版本(安卓的 versionName 也取它)。
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[derive(Deserialize)]
struct ReleaseDto {
    tag_name: String,
    assets: Vec<AssetDto>,
}

#[derive(Deserialize)]
struct AssetDto {
    name: String,
    browser_download_url: String,
}

/// 一个可装的新版。
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// 不带 `v` 的版本号。
    pub version: String,
    apk_name: String,
    apk_url: String,
    sha_url: String,
}

/// 查一次的结论。
#[derive(Debug, PartialEq)]
pub enum Check {
    UpToDate,
    Available(Update),
    /// 有更新的版本,但它的 APK 还没传上来(pc3 出包要十来分钟)。
    NotReady(String),
}

/// 问 GitHub 最新版,和本构建比。
pub async fn check() -> Result<Check, ApiError> {
    let bytes =
        platform::get_bytes(LATEST.to_owned()).await?;
    let release: ReleaseDto =
        serde_json::from_slice(&bytes)
            .map_err(|e| ApiError::Decode(e.to_string()))?;
    Ok(select(&release, current_version()))
}

/// 按 semver 比:0.1.9 早于 0.1.10,0.1.16-rc.1 早于 0.1.16。
fn parse(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(
        tag.strip_prefix('v').unwrap_or(tag),
    )
    .ok()
}

fn select(release: &ReleaseDto, current: &str) -> Check {
    // 读不懂的 tag 不当成更新:宁可不提示,也不装一个说不清是什么的包。
    let (Some(latest), Some(current)) =
        (parse(&release.tag_name), parse(current))
    else {
        return Check::UpToDate;
    };
    if latest <= current {
        return Check::UpToDate;
    }

    let version = latest.to_string();
    let apk_name =
        format!("osmosis-android-arm64-{version}.apk");
    let url_of = |name: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
    };
    match (
        url_of(&apk_name),
        url_of(&format!("{apk_name}.sha256")),
    ) {
        (Some(apk_url), Some(sha_url)) => {
            Check::Available(Update {
                version,
                apk_name,
                apk_url,
                sha_url,
            })
        }
        _ => Check::NotReady(version),
    }
}

/// 校验文件里那一行 `<64 位十六进制>  <文件名>`,文件名对得上才认。
fn expected_digest(
    sums: &str,
    apk_name: &str,
) -> Option<String> {
    let mut fields = sums.split_whitespace();
    let (digest, name) = (fields.next()?, fields.next()?);
    // `*` 是 sha256sum -b 的二进制标记。
    let valid = digest.len() == 64
        && digest.bytes().all(|b| b.is_ascii_hexdigit())
        && name.trim_start_matches('*') == apk_name;
    valid.then(|| digest.to_ascii_lowercase())
}

/// 下载到 `dir` 并核对。成功交出 APK 的路径;核不上的文件当场删掉。
///
/// `dir` 先整个清掉:里面只可能是上一次没装成的旧包,留着就是一百多 MB。
pub async fn fetch(
    update: &Update,
    dir: &Path,
    progress: impl Fn(u64, Option<u64>) + Send + 'static,
) -> Result<PathBuf, String> {
    let sums = platform::get_bytes(update.sha_url.clone())
        .await
        .map_err(|e| e.to_string())?;
    let expected = expected_digest(
        &String::from_utf8_lossy(&sums),
        &update.apk_name,
    )
    .ok_or("校验文件读不懂")?;

    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)
        .map_err(|e| e.to_string())?;
    let path = dir.join(&update.apk_name);
    let file = std::fs::File::create(&path)
        .map_err(|e| e.to_string())?;
    if let Err(e) = platform::download_anonymous(
        update.apk_url.clone(),
        file,
        progress,
    )
    .await
    {
        let _ = std::fs::remove_file(&path);
        return Err(e.to_string());
    }

    // 一百多 MB 的哈希,不放在 UI 线程上算。
    let checked = path.clone();
    platform::off_thread(move || {
        verify(&checked, &expected)
    })
    .await
    .ok_or("校验中途出错")??;
    Ok(path)
}

/// 核对 `path` 的 sha256,对不上就删掉它。
fn verify(
    path: &Path,
    expected: &str,
) -> Result<(), String> {
    use sha2::Digest as _;

    let actual =
        std::fs::File::open(path).and_then(|mut file| {
            let mut hasher = sha2::Sha256::new();
            std::io::copy(&mut file, &mut hasher)?;
            Ok(format!("{:x}", hasher.finalize()))
        });
    let why = match actual {
        Ok(actual) if actual == expected => return Ok(()),
        Ok(_) => "安装包校验不符,已丢弃".to_owned(),
        Err(e) => format!("读不了下载的安装包: {e}"),
    };
    let _ = std::fs::remove_file(path);
    Err(why)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "test" 的 sha256。
    const SHA: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    fn asset(name: &str) -> AssetDto {
        AssetDto {
            name: name.to_owned(),
            browser_download_url: format!(
                "https://example.test/{name}"
            ),
        }
    }

    /// 一个真实形状的 release:桌面资产、sha256sums.txt,外加这一版的两份 APK 资产。
    fn release(tag: &str, with_apk: bool) -> ReleaseDto {
        let ver = tag.trim_start_matches('v');
        let mut assets = vec![
            asset(&format!(
                "osmosis-{ver}-x86_64-linux.tar.gz"
            )),
            asset("sha256sums.txt"),
            // 别的版本的 APK 不能被挑中。
            asset("osmosis-android-arm64-0.0.1.apk"),
        ];
        if with_apk {
            assets.push(asset(&format!(
                "osmosis-android-arm64-{ver}.apk"
            )));
            assets.push(asset(&format!(
                "osmosis-android-arm64-{ver}.apk.sha256"
            )));
        }
        ReleaseDto {
            tag_name: tag.to_owned(),
            assets,
        }
    }

    #[test]
    fn a_newer_release_selects_its_own_apk_and_checksum() {
        let Check::Available(update) =
            select(&release("v0.1.17", true), "0.1.16")
        else {
            panic!("0.1.17 比 0.1.16 新,该给出更新");
        };
        assert_eq!(update.version, "0.1.17");
        assert_eq!(
            update.apk_url,
            "https://example.test/osmosis-android-arm64-0.1.17.apk"
        );
        assert_eq!(
            update.sha_url,
            "https://example.test/osmosis-android-arm64-0.1.17.apk.sha256"
        );
    }

    #[test]
    fn the_same_or_an_older_release_is_up_to_date() {
        assert_eq!(
            select(&release("v0.1.16", true), "0.1.16"),
            Check::UpToDate
        );
        assert_eq!(
            select(&release("v0.1.15", true), "0.1.16"),
            Check::UpToDate
        );
        // 数字比较,不是字符串比较:"0.1.9" 按字典序比 "0.1.10" 大。
        assert_eq!(
            select(&release("v0.1.9", true), "0.1.10"),
            Check::UpToDate
        );
    }

    #[test]
    fn pre_releases_order_below_their_final_version() {
        assert_eq!(
            select(
                &release("v0.1.16-rc.1", true),
                "0.1.16"
            ),
            Check::UpToDate,
            "0.1.16-rc.1 早于 0.1.16"
        );
        assert!(matches!(
            select(
                &release("v0.1.17-rc.1", true),
                "0.1.16"
            ),
            Check::Available(_)
        ));
        assert!(matches!(
            select(
                &release("v0.1.16", true),
                "0.1.16-rc.1"
            ),
            Check::Available(_)
        ));
    }

    #[test]
    fn a_newer_release_without_its_apk_is_not_ready() {
        assert_eq!(
            select(&release("v0.1.17", false), "0.1.16"),
            Check::NotReady("0.1.17".to_owned())
        );
    }

    #[test]
    fn an_unreadable_tag_never_offers_an_update() {
        assert_eq!(
            select(&release("nightly", true), "0.1.16"),
            Check::UpToDate
        );
    }

    #[test]
    fn the_checksum_line_must_name_the_apk() {
        let name = "osmosis-android-arm64-0.1.17.apk";
        assert_eq!(
            expected_digest(
                &format!("{SHA}  {name}\n"),
                name
            ),
            Some(SHA.to_owned())
        );
        // sha256sum -b 的二进制标记。
        assert_eq!(
            expected_digest(
                &format!("{SHA} *{name}"),
                name
            ),
            Some(SHA.to_owned())
        );
        assert_eq!(
            expected_digest(
                &format!(
                    "{SHA}  osmosis-android-arm64-0.1.16.apk"
                ),
                name
            ),
            None,
            "别的文件的哈希不能拿来核这一个"
        );
        assert_eq!(
            expected_digest(
                &format!("{}  {name}", &SHA[1..]),
                name
            ),
            None,
            "不足 64 位"
        );
        assert_eq!(
            expected_digest(
                &format!("{}g  {name}", &SHA[1..]),
                name
            ),
            None,
            "不是十六进制"
        );
    }

    fn temp_apk(tag: &str, body: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "osmosis-update-{tag}-{}.apk",
            std::process::id()
        ));
        std::fs::write(&path, body)
            .expect("写不了临时文件");
        path
    }

    #[test]
    fn a_matching_download_is_kept() {
        let path = temp_apk("ok", b"test");
        assert_eq!(verify(&path, SHA), Ok(()));
        assert!(path.exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_mismatching_download_is_discarded() {
        let path = temp_apk("bad", b"tampered");
        assert!(verify(&path, SHA).is_err());
        assert!(!path.exists(), "核不上的包必须删掉");
    }
}
