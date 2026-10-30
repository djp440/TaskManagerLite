//! CSV 编码（RFC 4180）与写文件，供“导出为 CSV”使用（issue #82）。
//!
//! 只做编码，不解析；不引入第三方 csv 库。约定：
//! - 字段含逗号、双引号、回车或换行时整体加双引号，其中的双引号写成两个；
//! - 行尾用 CRLF，最后一行之后也有 CRLF；
//! - 文件以 UTF-8 BOM 开头，Excel 才会按 UTF-8 打开（否则中文乱码）。

use std::borrow::Cow;
use std::path::Path;

use crate::error::SystemError;

/// UTF-8 字节序标记。
pub const BOM: &[u8] = b"\xEF\xBB\xBF";

/// 转义一个字段：需要时加引号。
pub fn escape_field(field: &str) -> Cow<'_, str> {
    if !field.contains([',', '"', '\r', '\n']) {
        return Cow::Borrowed(field);
    }
    Cow::Owned(format!("\"{}\"", field.replace('"', "\"\"")))
}

/// 一行（不含行尾）。
pub fn encode_row<S: AsRef<str>>(cells: &[S]) -> String {
    cells.iter().map(|c| escape_field(c.as_ref())).collect::<Vec<_>>().join(",")
}

/// 整张表的文件内容：BOM + 每行以 CRLF 结尾。
pub fn to_bytes<S: AsRef<str>>(rows: &[Vec<S>]) -> Vec<u8> {
    let mut out = BOM.to_vec();
    for row in rows {
        out.extend_from_slice(encode_row(row).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// 写成文件（覆盖已有文件）。失败时返回系统错误码。
pub fn write_file<S: AsRef<str>>(path: &Path, rows: &[Vec<S>]) -> Result<(), SystemError> {
    std::fs::write(path, to_bytes(rows)).map_err(SystemError::from)
}

/// 制表符分隔的一行文本（不含行尾），用于复制到剪贴板。
/// 制表符分隔文本没有引号规则，字段里的制表符与换行换成空格，保证一行、列数不变。
pub fn tab_line<S: AsRef<str>>(cells: &[S]) -> String {
    cells
        .iter()
        .map(|c| c.as_ref().replace(['\t', '\r', '\n'], " "))
        .collect::<Vec<_>>()
        .join("\t")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的最小 RFC 4180 解析器：只用来验证编码可以还原。
    fn parse(text: &str) -> Vec<Vec<String>> {
        let mut rows = Vec::new();
        let mut row = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match (quoted, c) {
                (true, '"') if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                (true, '"') => quoted = false,
                (true, c) => field.push(c),
                (false, '"') => quoted = true,
                (false, ',') => row.push(std::mem::take(&mut field)),
                (false, '\r') => {}
                (false, '\n') => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                (false, c) => field.push(c),
            }
        }
        assert!(!quoted, "引号没有闭合");
        if !field.is_empty() || !row.is_empty() {
            row.push(field);
            rows.push(row);
        }
        rows
    }

    #[test]
    fn plain_fields_are_left_alone() {
        assert_eq!(escape_field("notepad.exe"), "notepad.exe");
        assert_eq!(escape_field(""), "");
        assert_eq!(escape_field("12.3%"), "12.3%");
        assert_eq!(escape_field("C:\\Windows\\a b.exe"), "C:\\Windows\\a b.exe");
    }

    #[test]
    fn commas_quotes_and_newlines_are_quoted() {
        assert_eq!(escape_field("a,b"), "\"a,b\"");
        assert_eq!(escape_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_field("line1\nline2"), "\"line1\nline2\"");
        assert_eq!(escape_field("line1\r\nline2"), "\"line1\r\nline2\"");
        assert_eq!(escape_field("\""), "\"\"\"\"");
    }

    #[test]
    fn chinese_text_is_not_quoted_but_chinese_comma_stays_plain() {
        // 全角逗号不是分隔符，不需要引号。
        assert_eq!(encode_row(&["进程", "名称，全角"]), "进程,名称，全角");
        assert_eq!(encode_row(&["应用 (3)", "a,b"]), "应用 (3),\"a,b\"");
    }

    #[test]
    fn empty_fields_keep_their_columns() {
        assert_eq!(encode_row(&["", "x", ""]), ",x,");
        assert_eq!(encode_row::<&str>(&[]), "");
        assert_eq!(encode_row(&[""]), "");
    }

    #[test]
    fn bytes_start_with_bom_and_end_each_row_with_crlf() {
        let rows = vec![vec!["名称", "PID"], vec!["a,b", "4"]];
        let bytes = to_bytes(&rows);
        assert_eq!(&bytes[..3], BOM);
        let text = std::str::from_utf8(&bytes[3..]).unwrap();
        assert_eq!(text, "名称,PID\r\n\"a,b\",4\r\n");
        // BOM 之后不再有 BOM，且恰好 3 字节。
        assert_eq!(bytes.iter().take(3).copied().collect::<Vec<u8>>(), vec![0xEF, 0xBB, 0xBF]);
    }

    #[test]
    fn empty_table_is_just_the_bom() {
        assert_eq!(to_bytes::<&str>(&[]), BOM);
    }

    #[test]
    fn nasty_command_line_round_trips() {
        let cmd = "\"C:\\Program Files\\x.exe\" --a=\"1,2\" \"b\"\"c\"\r\n第二行, 中文";
        let rows = vec![vec!["名称".to_string(), "命令行".to_string()], vec!["x.exe".into(), cmd.into()], vec![String::new(), "尾".into()]];
        let bytes = to_bytes(&rows);
        let back = parse(std::str::from_utf8(&bytes[3..]).unwrap());
        assert_eq!(back, rows);
    }

    #[test]
    fn tab_line_flattens_separators() {
        assert_eq!(tab_line(&["a", "b\tc", "d\r\ne", ""]), "a\tb c\td  e\t");
        assert_eq!(tab_line(&["x,y", "\"q\""]), "x,y\t\"q\"");
    }

    #[test]
    fn write_file_writes_the_encoded_bytes_and_reports_missing_directory() {
        let dir = std::env::temp_dir().join(format!("tml-csv-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.csv");
        write_file(&path, &[vec!["名称", "x,y"]]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"\xEF\xBB\xBF\xE5\x90\x8D\xE7\xA7\xB0,\"x,y\"\r\n");
        let missing = dir.join("no-such-dir").join("a.csv");
        // 目录不存在：ERROR_PATH_NOT_FOUND (3)。
        assert_eq!(write_file(&missing, &[vec!["x"]]), Err(SystemError::Win32(3)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
