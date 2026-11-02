fn main() {
    glib_build_tools::compile_resources(
        &["resources", "../../assets"],
        "resources/booklub.gresource.xml",
        "booklub.gresource",
    );
}
