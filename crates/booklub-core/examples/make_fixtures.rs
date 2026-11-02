//! Writes the end-to-end fixtures: a workbook, and a JSON file shaped like a web build export.
//! Usage: make_fixtures <directory>

use rust_xlsxwriter::Workbook;

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("directory"));
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    let rows: [[&str; 5]; 5] = [
        ["Book Title", "Writer", "Topic", "State", "ISBN"],
        ["Ѓердан од ќерамиди", "Ќирил Ѓорѓиевски", "Design", "Unread", "9780140328721"],
        ["Second Row", "B", "Strategy / Pricing", "Done", ""],
        ["", "No title here", "Design", "Reading", ""],
        ["Third Row", "C", "Brand New Category", "Reading", "9780140328722"],
    ];
    for (row, cells) in rows.iter().enumerate() {
        for (column, text) in cells.iter().enumerate() {
            sheet.write_string(row as u32, column as u16, *text).expect("write");
        }
    }
    workbook.save(dir.join("reading-list.xlsx")).expect("save workbook");

    let web = r#"{"schemaVersion":1,"exportedAt":"2026-08-05T10:00:00.000Z",
      "categories":[{"id":"w2","name":"Strategy Web","subCategories":[{"id":"ws","name":"Pricing Web"}]},{"id":"w1","name":"Agile Web","subCategories":[]}],
      "goals":[{"id":"booksPerYear","target":30,"active":true},{"id":"booksPerMonth","target":3,"active":false}],
      "books":[
        {"id":"wb2","title":"Web Second","author":"A","categoryId":"w2","subCategoryId":"ws","priority":"High","status":"Reading","totalPages":100,"currentPage":50,"startDate":"2026-01-01","finishDate":null,"rating":4,"notes":"n","order":7,"addedAt":"2026-01-01T00:00:00.000Z"},
        {"id":"wb1","title":"Ѓердан од ќерамиди","author":"Ќирил","categoryId":null,"subCategoryId":null,"priority":"Low","status":"To Read","totalPages":null,"currentPage":0,"startDate":null,"finishDate":null,"rating":null,"notes":"","order":2,"addedAt":"2026-01-02T00:00:00.000Z"}
      ]}"#;
    std::fs::write(dir.join("web-export.json"), web).expect("write json");
}
