//! Minimal offline OpenXML workbook serialization.

use super::{Cell, Sheet};
use qx_app::artifact_store::sink::{self, PublishError};
use std::io::{self, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub(super) fn write_workbook(output: &Path, sheets: &[Sheet]) -> Result<(), String> {
    // XLSX 是 zip 流：走发布出口的流式档——fill 写完 fsync 一次、再原子发布，崩溃不留半份。
    sink::publish_stream(output, sink::PublishMode::Overwrite, |file| {
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        add_entry(
            &mut zip,
            "[Content_Types].xml",
            &content_types(sheets.len()),
            options,
        )?;
        add_entry(&mut zip, "_rels/.rels", ROOT_RELS, options)?;
        add_entry(&mut zip, "xl/workbook.xml", &workbook_xml(sheets), options)?;
        add_entry(
            &mut zip,
            "xl/_rels/workbook.xml.rels",
            &workbook_rels(sheets.len()),
            options,
        )?;
        for (index, (_, rows)) in sheets.iter().enumerate() {
            add_entry(
                &mut zip,
                &format!("xl/worksheets/sheet{}.xml", index + 1),
                &worksheet_xml(rows),
                options,
            )?;
        }
        zip.finish().map_err(|error| {
            PublishError::Io(io::Error::other(format!("完成 XLSX 工作簿失败: {error}")))
        })?;
        Ok(())
    })
    .map_err(|error| format!("发布 XLSX 报告失败 {}: {error}", output.display()))
}

fn add_entry<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    name: &str,
    content: &str,
    options: SimpleFileOptions,
) -> Result<(), PublishError> {
    zip.start_file(name, options).map_err(|error| {
        PublishError::Io(io::Error::other(format!(
            "创建 XLSX 条目 {name} 失败: {error}"
        )))
    })?;
    zip.write_all(content.as_bytes()).map_err(|error| {
        PublishError::Io(io::Error::other(format!(
            "写入 XLSX 条目 {name} 失败: {error}"
        )))
    })
}

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;

fn content_types(count: usize) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#,
    );
    for index in 1..=count {
        xml.push_str(&format!("<Override PartName=\"/xl/worksheets/sheet{index}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"));
    }
    xml.push_str("</Types>");
    xml
}

fn workbook_xml(sheets: &[Sheet]) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>"#,
    );
    for (index, (name, _)) in sheets.iter().enumerate() {
        xml.push_str(&format!(
            "<sheet name=\"{name}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
            index + 1,
            index + 1
        ));
    }
    xml.push_str("</sheets></workbook>");
    xml
}

fn workbook_rels(count: usize) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for index in 1..=count {
        xml.push_str(&format!("<Relationship Id=\"rId{index}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{index}.xml\"/>"));
    }
    xml.push_str("</Relationships>");
    xml
}

fn worksheet_xml(rows: &[Vec<Cell>]) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
    );
    for (row_index, row) in rows.iter().enumerate() {
        xml.push_str(&format!("<row r=\"{}\">", row_index + 1));
        for (column_index, cell) in row.iter().enumerate() {
            let reference = format!("{}{}", column_name(column_index + 1), row_index + 1);
            match cell {
                Cell::Text(text) => xml.push_str(&format!("<c r=\"{reference}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", xml_escape(text))),
                Cell::Number(number) => xml.push_str(&format!("<c r=\"{reference}\"><v>{}</v></c>", xml_escape(number))),
            }
        }
        xml.push_str("</row>");
    }
    xml.push_str("</sheetData></worksheet>");
    xml
}

fn column_name(mut number: usize) -> String {
    let mut name = String::new();
    while number > 0 {
        number -= 1;
        name.insert(0, (b'A' + (number % 26) as u8) as char);
        number /= 26;
    }
    name
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::new();
    for character in value
        .chars()
        .filter(|character| matches!(*character, '\t' | '\n' | '\r') || *character >= '\u{20}')
    {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}
