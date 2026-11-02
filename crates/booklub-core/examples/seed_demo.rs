//! Fills a library with demo data for screenshots and performance runs.
//! Usage: seed_demo <data-dir> <cache-dir> [book-count]
//! It only writes under the directories it is given.

use std::path::PathBuf;

use booklub_core::background::{self, check};
use booklub_core::paths::Paths;
use booklub_core::rules::{apply_page_change, apply_status_change};
use booklub_core::store::Store;
use booklub_core::model::{new_id, SubCategory};
use booklub_core::{Book, CategoryBackground, Priority, Status};
use image::{ImageFormat, Rgb, RgbImage};

const TITLES: &[(&str, &str)] = &[
    ("Drive", "Daniel H. Pink"),
    ("Good Strategy Bad Strategy", "Richard Rumelt"),
    ("Inspired", "Marty Cagan"),
    ("Thinking, Fast and Slow", "Daniel Kahneman"),
    ("Ѓердан од ќерамиди", "Ќирил Ѓорѓиевски"),
    ("7 Powers", "Hamilton Helmer"),
    ("The Very Long Title of a Book That Has to Wrap Without Breaking the Tile", "A. N. Author"),
    ("Obviously Awesome", "April Dunford"),
    ("Escaping the Build Trap", "Melissa Perri"),
    ("Playing to Win", "A. G. Lafley, Roger Martin"),
    ("Blue Ocean Strategy", "W. Chan Kim"),
    ("Monetizing Innovation", "Madhavan Ramanujam"),
    ("Competitive Strategy", "Michael E. Porter"),
    ("The Strategy Book", "Max McKeown"),
    ("Radical Candor", "Kim Scott"),
    ("Continuous Discovery Habits", "Teresa Torres"),
];

fn artwork(seed: u32) -> Vec<u8> {
    let palette = [[46, 74, 140], [169, 100, 10], [47, 122, 78], [122, 60, 150], [14, 77, 92], [190, 90, 70]];
    let a = palette[seed as usize % palette.len()];
    let b = palette[(seed as usize + 2) % palette.len()];
    let bright = seed % 11 == 0;
    let image = RgbImage::from_fn(600, 900, |x, y| {
        let t = (x as f32 / 600.0 + y as f32 / 900.0) / 2.0;
        let wave = (((x as f32 / 70.0).sin() + (y as f32 / 90.0).cos()) * 12.0) as i32;
        let mix = |i: usize| {
            let value = a[i] as f32 * (1.0 - t) + b[i] as f32 * t + wave as f32;
            let value = if bright { 255.0 - (255.0 - value) * 0.08 } else { value };
            value.clamp(0.0, 255.0) as u8
        };
        Rgb([mix(0), mix(1), mix(2)])
    });
    let mut bytes = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Jpeg).expect("encoding to memory");
    bytes
}

fn main() {
    let mut args = std::env::args().skip(1);
    let data = PathBuf::from(args.next().expect("data dir"));
    let cache = PathBuf::from(args.next().expect("cache dir"));
    let count: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(96);

    let paths = Paths::under(data.join("booklub"), cache.join("booklub"));
    paths.ensure().expect("dirs");
    let store = Store::open(&paths.database, &paths.backups).expect("store");
    let library = store.load_all().expect("load");
    assert!(library.books.is_empty(), "refusing to seed a library that already has books");
    let mut categories = library.categories;
    // The same two categories a new library starts with, filled out to show sub-categories and art.
    for (category, (subs, preset)) in categories.iter_mut().zip([(["Classics", "Thrillers"], "teal-geometry"), (["History", "Science"], "violet-bloom")]) {
        category.sub_categories = subs.iter().map(|sub| SubCategory { id: new_id(), name: (*sub).to_string() }).collect();
        category.background = Some(CategoryBackground::Preset(preset.to_string()));
    }
    for (position, category) in categories.iter().enumerate() {
        store.save_category(category, position).expect("category");
    }

    let mut books = Vec::with_capacity(count);
    for index in 0..count {
        let seed = index as u32;
        let (title, author) = TITLES[index % TITLES.len()];
        let mut book = Book::new(index as i64);
        book.title = if index < TITLES.len() { title.to_string() } else { format!("{title} {}", index / TITLES.len() + 1) };
        book.author = author.to_string();
        if index % 13 != 12 {
            let category = &categories[index % categories.len()];
            book.category_id = Some(category.id.clone());
            if !category.sub_categories.is_empty() && index % 3 != 0 {
                book.sub_category_id = Some(category.sub_categories[index % category.sub_categories.len()].id.clone());
            }
        }
        book.priority = match index % 5 { 0 => Priority::High, 4 => Priority::Low, _ => Priority::Medium };
        book.total_pages = Some(180 + (seed * 37) % 400);
        book.isbn = Some("9780140328721".to_string()).filter(|_| index == 0);
        book.publisher = "Demo Press".to_string();
        book.published_year = Some(1990 + (index % 35) as i32);
        book.language = if index == 4 { "Macedonian" } else { "English" }.to_string();
        match index % 7 {
            0 | 1 => {
                apply_page_change(&mut book, 20 + (seed * 11) % 150);
                book.start_date = Some(format!("2026-{:02}-{:02}", 1 + index % 10, 1 + index % 27));
            }
            2 | 3 => {
                book.start_date = Some(format!("2026-{:02}-01", 1 + index % 9));
                book.finish_date = Some(format!("2026-{:02}-{:02}", 1 + index % 9, 2 + index % 26));
                apply_status_change(&mut book, Status::Finished);
                book.rating = Some(1 + (index % 5) as u8);
            }
            _ => {}
        }
        if index % 2 == 0 {
            let checked = check(artwork(seed)).expect("demo art is valid");
            book.background_file = Some(background::store(&checked, &paths.backgrounds, &paths.thumbnails).expect("stored"));
        }
        books.push(book);
    }
    store.save_import(&books, &[]).expect("saved");
    println!("seeded {} books under {}", books.len(), paths.database.display());
}
