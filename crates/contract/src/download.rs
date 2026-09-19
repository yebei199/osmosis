//! 下载:落盘文件名的规则,以及只有试听片段时的错误码。
//!
//! 文件名放在契约里而不是各端各写一份,是因为它**出现在网络上** ——
//! 服务端把它写进 `Content-Disposition`,客户端按它落盘。两份实现迟早会分叉,
//! 而分叉的现象是手机上的文件名与服务端日志里的对不上,谁也不会想到去比对它。

/// 这条源只有试听片段,下不了整首。
///
/// 与 `forbidden` 分开:客户端要据此说清「这首歌要会员」,而不是笼统的"不让下"。
pub const TRIAL_ONLY: &str = "trial_only";

/// 落盘文件名里不许出现的字符,一律换成 `_`。
///
/// 取三套文件系统的并集而不是 Linux 那一套:文件要进手机的公共音乐目录,
/// 而那块盘上的卷可能是 FAT/exFAT,`:` 和 `?` 在那里建不出文件来。
const ILLEGAL: [char; 9] =
    ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// 文件名主干最多几个**字符**。
///
/// 按字符而不是字节:CJK 一个字三字节,按字节切会切在字符中间,
/// `String` 那一侧直接 panic。255 是常见的字节上限,留出余量给扩展名与重名后缀。
const MAX_STEM_CHARS: usize = 80;

/// 一首歌落盘叫什么:`<歌手> - <歌名>.mp3`。
///
/// 歌手列表用 `,` 拼 —— `/` 是路径分隔符,而歌手名里带斜杠并不罕见。
/// 一个歌手都没有时只剩歌名,不留一个孤零零的 ` - ` 前缀。
pub fn download_file_name(
    artists: &[String],
    title: &str,
) -> String {
    let artists = artists
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    let title = title.trim();

    let stem = if artists.is_empty() {
        title.to_owned()
    } else if title.is_empty() {
        artists
    } else {
        format!("{artists} - {title}")
    };

    let mut stem = sanitize(&stem);
    if stem.is_empty() {
        // 歌手与歌名都为空,或者整条被替换成了空白 —— 仍要给出一个能建出来的名字。
        stem.push_str("untitled");
    }

    format!("{stem}.mp3")
}

/// 把一段任意文本收拾成能当文件名的样子。
fn sanitize(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|ch| {
            // 控制字符同样建不出文件来,而歌名里混进 \n 是平台数据的常态。
            if ILLEGAL.contains(&ch) || ch.is_control() {
                '_'
            } else {
                ch
            }
        })
        .take(MAX_STEM_CHARS)
        .collect();

    // 结尾的点与空格在 Windows 上会被静默吃掉,于是两首歌重名。
    cleaned.trim_end_matches(['.', ' ']).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usual_shape_is_artist_dash_title() {
        assert_eq!(
            download_file_name(
                &["Aimer".to_owned()],
                "残響散歌"
            ),
            "Aimer - 残響散歌.mp3"
        );
    }

    /// 多个歌手用 `,` 拼:`/` 在文件名里是路径分隔符,拼进去等于要求建一层目录。
    #[test]
    fn several_artists_join_with_a_comma() {
        assert_eq!(
            download_file_name(
                &["A".to_owned(), "B".to_owned()],
                "t"
            ),
            "A,B - t.mp3"
        );
    }

    /// 没有歌手时不留孤零零的 ` - ` 前缀。
    #[test]
    fn no_artist_leaves_no_dangling_separator() {
        assert_eq!(
            download_file_name(&[], "纯音乐"),
            "纯音乐.mp3"
        );
        assert_eq!(
            download_file_name(
                &["  ".to_owned()],
                "纯音乐"
            ),
            "纯音乐.mp3"
        );
    }

    /// 路径分隔符与控制字符要换掉,否则落盘那一步会去建一层不存在的目录,
    /// 或者干脆建不出文件。
    #[test]
    fn path_separators_and_control_chars_are_replaced() {
        assert_eq!(
            download_file_name(
                &["AC/DC".to_owned()],
                "Back\nIn: Black?"
            ),
            "AC_DC - Back_In_ Black_.mp3"
        );
    }

    /// 长歌名按**字符**截断。按字节切会切在 CJK 字符中间并直接 panic。
    #[test]
    fn a_long_name_is_cut_on_a_char_boundary() {
        let title = "残".repeat(200);
        let name = download_file_name(&[], &title);

        assert_eq!(
            name.chars().count(),
            MAX_STEM_CHARS + ".mp3".len(),
            "主干应当正好截到上限"
        );
    }

    /// 结尾的点与空格在 Windows 上会被静默吃掉,于是两首本不同名的歌重名。
    #[test]
    fn trailing_dots_and_spaces_are_dropped() {
        assert_eq!(
            download_file_name(&[], "结局. "),
            "结局.mp3"
        );
    }

    /// 什么都没有时也要给得出一个建得出来的名字 —— 空文件名会让落盘那一步失败,
    /// 而那时的报错离「这首歌没有标题」很远。
    #[test]
    fn an_empty_track_still_gets_a_name() {
        assert_eq!(
            download_file_name(&[], "   "),
            "untitled.mp3"
        );
    }
}
