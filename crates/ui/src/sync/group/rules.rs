//! 播放组那几句文案与判断(#142)。拿出来是为了不起窗口就能测,也让字体守卫盖得到。

use std::collections::HashMap;

use app_core::{DeviceReportDto, OutputRouteDto, Standing};

/// 没做过声学校准的路由(蓝牙、有线/USB)上的出声设备,组那一行这样标(#137 ⑤ 冻结的合同)。
pub const UNCALIBRATED: &str = "该路由未校准,不保证同步";

/// 界面上写死在 `.slint` 里的组相关文案,在这里各留一份,让字体守卫盖得到。
///
/// 出处:`slint/controls.slint` 的输出设备一行,`slint/app.slint` 的组横幅。
#[cfg(test)]
pub const SLINT_COPY: &[&str] =
    &["输出设备", "本机", "退出", "加入", "移出"];

/// 组横幅那一句。不在组里是空串 —— 空串就是不显示。
///
/// `fellows` 是组里别的出声设备的名字。
pub fn describe_banner(
    standing: Standing,
    fellows: &[String],
) -> String {
    match standing {
        Standing::Solo => String::new(),
        Standing::Output if fellows.is_empty() => {
            "在组里播放".to_owned()
        }
        Standing::Output => {
            format!("与 {} 一起播放", fellows.join("、"))
        }
        Standing::Remote if fellows.is_empty() => {
            "组里暂时没有设备出声".to_owned()
        }
        Standing::Remote => {
            format!("正在遥控 {}", fellows.join("、"))
        }
    }
}

/// 输出设备那一行。不在组里就是本机;在组里列出正在出声的那几台。
pub fn describe_output(outputs: &[String]) -> String {
    if outputs.is_empty() {
        return "输出: 本机".to_owned();
    }
    format!("输出: {}", outputs.join("、"))
}

/// 组那一行:各台出声设备报上来的故障,以及没校准过的路由。没有要说的就是空串。
///
/// `names` 把设备 id 换成名字,本机的 id 换成「本机」。
pub fn describe_group(
    outputs: &[String],
    reports: &HashMap<String, DeviceReportDto>,
    name: impl Fn(&str) -> String,
) -> String {
    let mut parts = Vec::new();
    for id in outputs {
        let Some(report) = reports.get(id) else {
            continue;
        };
        if let Some(why) = &report.fault {
            parts.push(format!("{}: {why}", name(id)));
        }
        if outputs.len() > 1
            && matches!(
                report.route,
                Some(
                    OutputRouteDto::Bluetooth
                        | OutputRouteDto::Wired
                )
            )
        {
            parts.push(format!(
                "{}: {UNCALIBRATED}",
                name(id)
            ));
        }
    }
    parts.join(";")
}

/// 一条意图没成时那句话:做的是什么、为什么。
pub fn describe_intent_failure(
    what: &str,
    why: &str,
) -> String {
    format!("没能{what}: {why}")
}

/// 出声设备取不到全局状态要的那一首时报的故障。它不自己从头放、不换下一首。
pub fn describe_media_fault(why: &str) -> String {
    format!("取不到媒体: {why}")
}

/// 出声设备手上那一版队列里没有状态要的那一条。
pub fn describe_missing_entry(
    revision: i64,
    entry_id: i64,
) -> String {
    format!("第 {revision} 版里没有条目 {entry_id}")
}

/// 出声设备取不下状态那一版的队列。
pub fn describe_copy_fault(why: &str) -> String {
    format!("队列没取下来: {why}")
}

/// 音频层查到的输出路由换成线上格式。
pub(crate) fn route_dto(
    route: audio::Route,
) -> OutputRouteDto {
    match route {
        audio::Route::Speaker => OutputRouteDto::Speaker,
        audio::Route::Bluetooth => {
            OutputRouteDto::Bluetooth
        }
        audio::Route::Wired => OutputRouteDto::Wired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// 出声的说「与谁一起」,只当遥控器的说「在遥控谁」,组外不显示。
    #[test]
    fn the_banner_says_who_plays_along() {
        assert_eq!(
            describe_banner(
                Standing::Output,
                &names(&["pc1"])
            ),
            "与 pc1 一起播放"
        );
        assert_eq!(
            describe_banner(
                Standing::Remote,
                &names(&["pc1", "小米"])
            ),
            "正在遥控 pc1、小米"
        );
        assert_eq!(
            describe_banner(Standing::Output, &[]),
            "在组里播放"
        );
        assert_eq!(
            describe_banner(Standing::Solo, &[]),
            ""
        );
    }

    /// 输出设备那一行列出正在出声的那几台。
    #[test]
    fn the_output_line_lists_the_sounding_devices() {
        assert_eq!(describe_output(&[]), "输出: 本机");
        assert_eq!(
            describe_output(&names(&["本机", "pc1"])),
            "输出: 本机、pc1"
        );
    }

    /// 组那一行只说故障与没校准的路由;一台出声时路由不算问题。
    #[test]
    fn the_group_line_reports_faults_and_uncalibrated_routes()
     {
        let reports = HashMap::from([
            (
                "pc".to_owned(),
                DeviceReportDto {
                    entry_id: Some(1),
                    fault: Some(
                        "取不到媒体: 403".to_owned(),
                    ),
                    route: Some(OutputRouteDto::Speaker),
                },
            ),
            (
                "phone".to_owned(),
                DeviceReportDto {
                    entry_id: Some(1),
                    fault: None,
                    route: Some(OutputRouteDto::Bluetooth),
                },
            ),
        ]);
        let name = |id: &str| id.to_owned();

        assert_eq!(
            describe_group(
                &names(&["pc", "phone"]),
                &reports,
                name
            ),
            format!(
                "pc: 取不到媒体: 403;phone: {UNCALIBRATED}"
            )
        );
        assert_eq!(
            describe_group(
                &names(&["phone"]),
                &reports,
                name
            ),
            ""
        );
    }

    /// 本模块吐出的中文必须在子集字体里。新增文案忘了重跑 `just font-subset` 就红。
    #[test]
    fn group_copy_only_uses_subset_glyphs() {
        const CJK_SUBSET: &[u8] =
            include_bytes!("../../../fonts/cjk-subset.ttf");
        let face = ttf_parser::Face::parse(CJK_SUBSET, 0)
            .expect("子集字体应能被解析");

        let mut copy: Vec<String> = SLINT_COPY
            .iter()
            .map(|line| (*line).to_owned())
            .collect();
        for standing in [
            Standing::Output,
            Standing::Remote,
            Standing::Solo,
        ] {
            copy.push(describe_banner(standing, &[]));
            copy.push(describe_banner(
                standing,
                &names(&["a", "b"]),
            ));
        }
        copy.push(describe_output(&[]));
        copy.push(describe_output(&names(&["a", "b"])));
        copy.push(UNCALIBRATED.to_owned());
        copy.push(describe_intent_failure("点歌", "x"));
        for what in [
            "点歌",
            "切换",
            "改输出设备",
            "退出组",
            "这首已经在放或正在切过去",
            "连不上服务端,本机已暂停",
            "本机",
        ] {
            copy.push(what.to_owned());
        }
        copy.push(describe_media_fault("x"));
        copy.push(describe_missing_entry(1, 2));
        copy.push(describe_copy_fault("x"));

        let missing: Vec<char> = copy
            .iter()
            .flat_map(|line| line.chars())
            .filter(|c| {
                !c.is_ascii()
                    && face.glyph_index(*c).is_none()
            })
            .collect();
        assert!(
            missing.is_empty(),
            "子集字体缺字形:{missing:?} —— 重跑 just font-subset"
        );
    }
}
