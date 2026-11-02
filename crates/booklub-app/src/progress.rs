//! The Analytics screen: goals and statistics (Sections 3.5 and 3.6).

use std::rc::Rc;

use adw::prelude::*;
use booklub_core::goals::{self, Pacing};
use booklub_core::model::GoalId;
use booklub_core::stats;
use booklub_core::{Goal, Priority, Status};
use chrono::{Datelike, Local};

use crate::app::App;
use crate::util::{books, hbox, icon, label, progress_bar, text_button, vbox};
use crate::window::refresh;

const MONTHS: [&str; 12] = ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"];
const BAR_MAX_HEIGHT: i32 = 96;
const HBAR_MAX_WIDTH: i32 = 190;
const MONTH_NAMES: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn pace_chip(pacing: Pacing) -> gtk::Box {
    let (class, glyph) = match pacing {
        Pacing::Ahead => ("ahead", "pace-ahead"),
        Pacing::OnTrack => ("on-track", "pace-on-track"),
        Pacing::Behind => ("behind", "pace-behind"),
    };
    let chip = hbox(6);
    chip.add_css_class("pace-chip");
    chip.add_css_class(class);
    chip.set_halign(gtk::Align::Start);
    chip.set_valign(gtk::Align::Center);
    chip.append(&icon(glyph));
    chip.append(&gtk::Label::new(Some(pacing.label())));
    chip
}

fn card(heading: &str) -> gtk::Box {
    let root = vbox(10);
    root.add_css_class("card");
    root.add_css_class("card-pad");
    root.set_hexpand(true);
    if !heading.is_empty() {
        root.append(&label(heading, &["heading"]));
    }
    root
}

/// REQ-14, REQ-14a, REQ-14d, REQ-14e, REQ-14g: the target editor.
fn edit_goal(app: &Rc<App>, goal: Goal, anchor: &gtk::Button) {
    let body = vbox(8);
    body.set_margin_top(4);
    body.set_margin_bottom(4);
    body.set_margin_start(4);
    body.set_margin_end(4);
    body.append(&label("Target", &["field-label"]));
    let entry = gtk::Entry::builder().text(goal.target.to_string()).css_classes(["field-box"]).width_chars(8).build();
    entry.update_property(&[gtk::accessible::Property::Label("Goal target")]);
    body.append(&entry);
    let error = label("Enter a whole number of 1 or more.", &["field-error"]);
    error.set_visible(false);
    body.append(&error);
    let actions = hbox(8);
    let deactivate = text_button("Deactivate", "flat-text");
    let save = text_button("Save", "commit");
    actions.append(&deactivate);
    actions.append(&save);
    body.append(&actions);

    let popover = gtk::Popover::builder().child(&body).build();
    popover.set_parent(anchor);

    let commit = {
        let (app, popover, entry, error) = (app.clone(), popover.clone(), entry.clone(), error.clone());
        move || match entry.text().trim().parse::<u32>() {
            Ok(target) if target > 0 => {
                popover.popdown();
                if app.save_goal(Goal { target, ..goal }) {
                    refresh(&app);
                }
            }
            _ => error.set_visible(true),
        }
    };
    let commit = Rc::new(commit);
    let commit_clone = commit.clone();
    save.connect_clicked(move |_| commit_clone());
    entry.connect_activate(move |_| commit());

    let (app_clone, popover_clone) = (app.clone(), popover.clone());
    deactivate.connect_clicked(move |_| {
        popover_clone.popdown();
        if app_clone.save_goal(Goal { active: false, ..goal }) {
            refresh(&app_clone);
        }
    });
    popover.popup();
}

fn goal_card(app: &Rc<App>, goal: Goal) -> gtk::Box {
    let now = Local::now().naive_local();
    let period_name = match goal.id {
        GoalId::BooksPerYear => format!("{} \u{b7} Books per year", now.year()),
        GoalId::BooksPerMonth => format!("{} \u{b7} Books per month", MONTH_NAMES[now.month0() as usize]),
    };
    let root = card("");

    if !goal.active {
        // REQ-14e
        let top = hbox(8);
        let name = label(&period_name, &["heading"]);
        name.set_hexpand(true);
        top.append(&name);
        top.append(&label("INACTIVE", &["chip-text", "ink-2"]));
        root.append(&top);
        root.append(&label(&format!("The target of {} is not being tracked.", goal.target), &["caption"]));
        let activate = text_button("Activate", "secondary");
        activate.set_halign(gtk::Align::Start);
        let app_clone = app.clone();
        activate.connect_clicked(move |_| {
            if app_clone.save_goal(Goal { active: true, ..goal }) {
                refresh(&app_clone);
            }
        });
        root.append(&activate);
        return root;
    }

    let progress = {
        let library = app.library.borrow();
        goals::goal_progress(&goal, &library.books, now)
    };

    let top = hbox(8);
    let name = label(&period_name, &["heading"]);
    name.set_hexpand(true);
    top.append(&name);
    top.append(&pace_chip(progress.pacing));
    root.append(&top);

    let numbers = hbox(8);
    numbers.append(&label(&progress.achieved.to_string(), &["stat"]));
    let of = gtk::Button::builder().label(format!("of {} \u{b7} {}%", books(progress.target as usize), progress.percent)).css_classes(["goal-of"]).tooltip_text("Change this goal").valign(gtk::Align::Center).build();
    let (app_clone, goal_clone) = (app.clone(), goal);
    of.connect_clicked(move |button| edit_goal(&app_clone, goal_clone, button));
    numbers.append(&of);
    root.append(&numbers);

    let tone = match progress.pacing {
        Pacing::Behind => "warning",
        _ => "success",
    };
    root.append(&progress_bar(progress.percent, tone));

    let meta = hbox(16);
    meta.append(&label(&format!("{} remaining", progress.remaining), &["caption"]));
    meta.append(&label(&format!("{:.1} books per week needed", progress.per_week_needed), &["caption"]));
    let (period_start, period_end) = goals::goal_period(goal.id, now);
    let total_days = (period_end - period_start).num_days();
    let day = (now.date() - period_start).num_days() + 1;
    meta.append(&label(
        &match goal.id {
            GoalId::BooksPerYear => format!("{}% of year elapsed", day * 100 / total_days),
            GoalId::BooksPerMonth => format!("Day {day} of {total_days}"),
        },
        &["caption"],
    ));
    root.append(&meta);
    root
}

fn stat_tile(caption: &str, value: &str) -> gtk::Box {
    let tile = vbox(4);
    tile.add_css_class("stat-tile");
    tile.set_hexpand(true);
    tile.append(&label(caption, &["caption"]));
    tile.append(&label(value, &["stat-small"]));
    tile
}

fn month_chart(data: &stats::LibraryStats, year: i32) -> gtk::Box {
    let root = card(&format!("Finished per month \u{b7} {year}"));
    let peak = data.finished_by_month.iter().copied().max().unwrap_or(0).max(1);
    let chart = hbox(10);
    chart.set_homogeneous(true);
    chart.set_margin_top(6);
    for (index, count) in data.finished_by_month.iter().copied().enumerate() {
        let column = vbox(4);
        column.set_valign(gtk::Align::End);
        let value = label(&if count > 0 { count.to_string() } else { String::new() }, &["caption-2"]);
        value.set_xalign(0.5);
        column.append(&value);
        let bar = gtk::Box::builder().css_classes(["bar"]).build();
        if count == 0 {
            bar.add_css_class("empty");
        }
        bar.set_size_request(-1, (count as i32 * BAR_MAX_HEIGHT / peak as i32).max(1));
        column.append(&bar);
        let month = label(MONTHS[index], &["caption"]);
        month.set_xalign(0.5);
        column.append(&month);
        chart.append(&column);
    }
    chart.set_size_request(-1, BAR_MAX_HEIGHT + 44);
    chart.update_property(&[gtk::accessible::Property::Label(&format!(
        "Books finished per month in {year}: {}",
        data.finished_by_month.iter().enumerate().map(|(i, n)| format!("{} {n}", MONTH_NAMES[i])).collect::<Vec<_>>().join(", ")
    ))]);
    root.append(&chart);
    root
}

/// REQ-16g: every entry is labelled directly, so the chart never relies on its colours.
fn distribution(data: &stats::LibraryStats) -> gtk::Box {
    let root = card("Category distribution");
    let peak = data.by_category.iter().map(|c| c.count).max().unwrap_or(0).max(1);
    let grid = gtk::Grid::builder().row_spacing(10).column_spacing(12).build();
    for (row, entry) in data.by_category.iter().enumerate() {
        let name = label(&entry.name, &["caption-2"]);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_width_chars(16);
        name.set_max_width_chars(16);
        grid.attach(&name, 0, row as i32, 1, 1);
        let bars = hbox(8);
        let bar = gtk::Box::builder().css_classes(["hbar", format!("chart-{}", row % 5 + 1).as_str()]).valign(gtk::Align::Center).build();
        bar.set_size_request((entry.count as i32 * HBAR_MAX_WIDTH / peak as i32).max(1), 10);
        bars.append(&bar);
        bars.append(&label(&entry.count.to_string(), &["caption-2"]));
        grid.attach(&bars, 1, row as i32, 1, 1);
    }
    let last = data.by_category.len() as i32;
    grid.attach(&label("No category", &["caption"]), 0, last, 1, 1);
    grid.attach(&label(&books(data.uncategorized), &["caption"]), 1, last, 1, 1);
    root.append(&grid);

    // REQ-16j
    if !data.thinnest.is_empty() {
        let row = hbox(8);
        row.set_margin_top(4);
        let warning = icon("warning");
        warning.add_css_class("warning-ink");
        row.append(&warning);
        let names: Vec<&str> = data.thinnest.iter().map(|c| c.name.as_str()).collect();
        let text = label(&format!("Fewest books: {}", names.join(", ")), &["caption-2"]);
        text.set_wrap(true);
        row.append(&text);
        root.append(&row);
    }
    root
}

/// REQ-16h
fn priority_table(data: &stats::LibraryStats) -> gtk::Box {
    let root = card("Priority by status");
    let grid = gtk::Grid::builder().column_spacing(16).row_spacing(6).column_homogeneous(true).build();
    let columns = [Status::Reading, Status::ToRead, Status::Finished];
    for (index, status) in columns.iter().enumerate() {
        let heading = label(status.as_str(), &["caption"]);
        heading.set_xalign(1.0);
        grid.attach(&heading, index as i32 + 1, 0, 1, 1);
    }
    for (row, priority) in Priority::ALL.iter().enumerate() {
        grid.attach(&label(priority.as_str(), &["caption-2"]), 0, row as i32 + 1, 1, 1);
        for (column, status) in columns.iter().enumerate() {
            let count = data.priority_by_status.get(&(*priority, *status)).copied().unwrap_or(0);
            let value = label(&count.to_string(), &["body-strong"]);
            value.set_xalign(1.0);
            grid.attach(&value, column as i32 + 1, row as i32 + 1, 1, 1);
        }
    }
    root.append(&grid);
    root
}

/// REQ-16i, REQ-16k
fn rating_card(app: &Rc<App>, data: &stats::LibraryStats) -> gtk::Box {
    let root = card("Mean rating");
    let rated = app.library.borrow().books.iter().filter(|book| book.rating.is_some()).count();
    match data.mean_rating {
        Some(mean) => {
            root.append(&label(&format!("{mean:.1}"), &["stat"]));
            root.append(&label(&format!("across {} rated", books(rated)), &["caption"]));
        }
        None => {
            root.append(&label("\u{2014}", &["stat"]));
            root.append(&label("No books have a rating yet.", &["caption"]));
        }
    }
    root
}

fn two_columns(left: &gtk::Box, right: &gtk::Box) -> gtk::FlowBox {
    let flow = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(16).row_spacing(16).min_children_per_line(1).max_children_per_line(2).homogeneous(true).valign(gtk::Align::Start).build();
    left.set_valign(gtk::Align::Fill);
    right.set_valign(gtk::Align::Fill);
    flow.append(left);
    flow.append(right);
    for child in [flow.first_child(), flow.last_child()].into_iter().flatten() {
        child.set_focusable(false);
    }
    flow
}

pub fn rebuild(app: &Rc<App>) {
    let ui = app.ui();
    let now = Local::now().naive_local();
    let (goal_list, data) = {
        let library = app.library.borrow();
        (library.goals.clone(), stats::compute(&library.books, &library.categories, now))
    };

    let page = vbox(16);
    page.set_margin_top(20);
    page.set_margin_bottom(24);
    page.set_margin_start(24);
    page.set_margin_end(24);

    // Goals list yearly first (REQ-15h), which is how the store returns them.
    let yearly = goal_list.iter().find(|goal| goal.id == GoalId::BooksPerYear).copied();
    let monthly = goal_list.iter().find(|goal| goal.id == GoalId::BooksPerMonth).copied();
    if let (Some(yearly), Some(monthly)) = (yearly, monthly) {
        page.append(&two_columns(&goal_card(app, yearly), &goal_card(app, monthly)));
    }

    // The legend reads the pacing chips for anyone new to them (QA-35).
    let legend = hbox(8);
    legend.append(&label("Pacing:", &["caption"]));
    for pacing in [Pacing::Ahead, Pacing::OnTrack, Pacing::Behind] {
        legend.append(&pace_chip(pacing));
    }
    page.append(&legend);

    let status = |s: Status| data.by_status.get(&s).copied().unwrap_or(0).to_string();
    let tiles = gtk::FlowBox::builder().selection_mode(gtk::SelectionMode::None).column_spacing(10).row_spacing(10).min_children_per_line(2).max_children_per_line(7).homogeneous(true).valign(gtk::Align::Start).build();
    let values = [
        ("Total books", data.total.to_string()),
        ("Reading", status(Status::Reading)),
        ("To Read", status(Status::ToRead)),
        ("Finished", status(Status::Finished)),
        (&*format!("Finished in {}", now.year()), data.finished_this_year.to_string()),
        ("Avg days to finish", data.mean_days_to_finish.map_or("\u{2014}".to_string(), |days| format!("{}", days.round() as i64))),
        ("Avg rating", data.mean_rating.map_or("\u{2014}".to_string(), |rating| format!("{rating:.1}"))),
    ];
    for (caption, value) in values {
        let child = gtk::FlowBoxChild::builder().child(&stat_tile(caption, &value)).focusable(false).build();
        tiles.append(&child);
    }
    page.append(&tiles);

    page.append(&two_columns(&month_chart(&data, now.year()), &distribution(&data)));
    page.append(&two_columns(&priority_table(&data), &rating_card(app, &data)));

    ui.progress_page.set_child(Some(&page));
}
