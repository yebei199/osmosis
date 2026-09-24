//! 应用内升级(#129)的取数一侧:问 GitHub 最新版、挑出 APK、下载并按 sha256 核对。
//!
//! 装是平台的事(安卓走 PackageInstaller,见 `apps/android`),这一层只交出一个
//! 核对过的文件。资产名与校验文件的格式由 `release/README.md`「APK 资产约定」定。
//!
//! 版本与哈希都从 api.github.com 那一份 release JSON 里读,**不带登录态**(把我们的 token
//! 发过去就是泄露)。哈希取 APK 资产自带的 `digest`(GitHub 自己算的 `sha256:<hex>`),不去下
//! `.sha256` 文件:资产下载会 302 到 GitHub 的资产 CDN,手机网络上连不上(#129 真机实测 60 秒
//! 超时),而 api.github.com 秒回。
//! APK 字节从我们自己的服务端取(`/app/android/{ver}.apk`,见 `server/src/routes/apk.rs`)。
//! 服务端被换了包也没用,哈希是 GitHub 给的。

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
    /// `sha256:<64 位小写十六进制>`。GitHub 2025 年起给每个资产都算;更早上传的没有。
    digest: Option<String>,
}

/// 一个可装的新版。
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// 不带 `v` 的版本号。
    pub version: String,
    apk_name: String,
    /// APK 的 sha256,64 位小写十六进制。
    sha256: String,
}

impl Update {
    /// 字节从哪取:我们的服务端,它去 GitHub 回源。
    fn apk_url(&self) -> String {
        format!(
            "{}/app/android/{}.apk",
            crate::base_url(),
            self.version
        )
    }
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
    // APK 资产本身不从这里下,但它得在(服务端就是去拉它),而且得带着能用的哈希。
    // 缺哈希不退回去下 `.sha256`,也不改成信任服务端:当它还没就绪。
    let sha256 = release
        .assets
        .iter()
        .find(|asset| asset.name == apk_name)
        .and_then(|asset| {
            sha256_of(asset.digest.as_deref()?)
        });
    match sha256 {
        Some(sha256) => Check::Available(Update {
            version,
            apk_name,
            sha256,
        }),
        None => Check::NotReady(version),
    }
}

/// 从 `sha256:<hex>` 里取出 64 位十六进制,统一成小写。别的算法、长度不对、不是十六进制都是 `None`。
fn sha256_of(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64
        && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    .then(|| hex.to_ascii_lowercase())
}

/// 下载到 `dir` 并核对。成功交出 APK 的路径;核不上的文件当场删掉。
///
/// `dir` 先整个清掉:里面只可能是上一次没装成的旧包,留着就是一百多 MB。
pub async fn fetch(
    update: &Update,
    dir: &Path,
    progress: impl Fn(u64, Option<u64>) + Send + 'static,
) -> Result<PathBuf, String> {
    let expected = update.sha256.clone();
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)
        .map_err(|e| e.to_string())?;
    let path = dir.join(&update.apk_name);
    let file = std::fs::File::create(&path)
        .map_err(|e| e.to_string())?;
    if let Err(e) =
        platform::download(update.apk_url(), file, progress)
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
            digest: Some(format!("sha256:{SHA}")),
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

    /// 这一版的 APK 资产换上指定的 digest。
    fn with_apk_digest(
        tag: &str,
        digest: Option<&str>,
    ) -> ReleaseDto {
        let mut release = release(tag, true);
        let ver = tag.trim_start_matches('v');
        let apk =
            format!("osmosis-android-arm64-{ver}.apk");
        for asset in &mut release.assets {
            if asset.name == apk {
                asset.digest = digest.map(str::to_owned);
            }
        }
        release
    }

    #[test]
    fn a_newer_release_selects_its_own_apk_and_checksum() {
        let Check::Available(update) =
            select(&release("v0.1.17", true), "0.1.16")
        else {
            panic!("0.1.17 比 0.1.16 新,该给出更新");
        };
        assert_eq!(update.version, "0.1.17");
        // 字节走我们的服务端(国内直连 GitHub 资产只有几十 KB/s),哈希仍取 GitHub 的。
        assert_eq!(
            update.apk_url(),
            format!(
                "{}/app/android/0.1.17.apk",
                crate::base_url()
            )
        );
        assert_eq!(update.sha256, SHA);
    }

    /// api.github.com 真实回包的形状(节选,多余字段照留):哈希从 APK 那个资产的
    /// `digest` 里取,别的资产的 digest 不能串过来。
    #[test]
    fn the_digest_comes_from_the_release_json() {
        let apk_hex = "a".repeat(64);
        let json = format!(
            r#"{{
                "tag_name": "v0.1.17",
                "name": "v0.1.17",
                "assets": [
                    {{
                        "name": "osmosis-desktop-x86_64-linux",
                        "size": 1,
                        "digest": "sha256:{other}",
                        "browser_download_url": "https://github.com/x/y"
                    }},
                    {{
                        "name": "osmosis-android-arm64-0.1.17.apk",
                        "content_type": "application/vnd.android.package-archive",
                        "size": 58017267,
                        "digest": "sha256:{apk}",
                        "browser_download_url": "https://github.com/x/z"
                    }}
                ]
            }}"#,
            other = "b".repeat(64),
            apk = apk_hex.to_uppercase(),
        );
        let release: ReleaseDto =
            serde_json::from_str(&json)
                .expect("真实形状的 JSON 该解得出来");

        let Check::Available(update) =
            select(&release, "0.1.16")
        else {
            panic!("APK 带着 digest,该给出更新");
        };
        assert_eq!(update.sha256, apk_hex, "统一成小写");
    }

    #[test]
    fn an_apk_without_a_digest_is_not_ready() {
        assert_eq!(
            select(
                &with_apk_digest("v0.1.17", None),
                "0.1.16"
            ),
            Check::NotReady("0.1.17".to_owned())
        );
        // JSON 里干脆没有这个字段,也是同一个结论。
        let release: ReleaseDto = serde_json::from_str(
            r#"{"tag_name":"v0.1.17","assets":[
                {"name":"osmosis-android-arm64-0.1.17.apk"}]}"#,
        )
        .expect("缺 digest 的 JSON 也该解得出来");
        assert_eq!(
            select(&release, "0.1.16"),
            Check::NotReady("0.1.17".to_owned())
        );
    }

    #[test]
    fn a_malformed_digest_is_not_ready() {
        for bad in [
            format!("sha512:{SHA}"),
            SHA.to_owned(),
            format!("sha256:{}", &SHA[1..]),
            format!("sha256:{}g", &SHA[1..]),
            format!("sha256:{SHA}0"),
            "sha256:".to_owned(),
        ] {
            assert_eq!(
                select(
                    &with_apk_digest("v0.1.17", Some(&bad)),
                    "0.1.16"
                ),
                Check::NotReady("0.1.17".to_owned()),
                "{bad} 不该当成能用的哈希"
            );
        }
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

    /// 独有临时目录里的一个 apk。目录随返回的 `TempDir` 一起删。
    fn temp_apk(
        body: &[u8],
    ) -> (tempfile::TempDir, PathBuf) {
        let dir =
            tempfile::tempdir().expect("建不出临时目录");
        let path = dir.path().join("osmosis.apk");
        std::fs::write(&path, body)
            .expect("写不了临时文件");
        (dir, path)
    }

    #[test]
    fn a_matching_download_is_kept() {
        let (_dir, path) = temp_apk(b"test");
        assert_eq!(verify(&path, SHA), Ok(()));
        assert!(path.exists());
    }

    #[test]
    fn a_mismatching_download_is_discarded() {
        let (_dir, path) = temp_apk(b"tampered");
        assert!(verify(&path, SHA).is_err());
        assert!(!path.exists(), "核不上的包必须删掉");
    }
}
