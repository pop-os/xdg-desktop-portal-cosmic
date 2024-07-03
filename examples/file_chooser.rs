use ashpd::desktop::file_chooser::{Choice, FileFilter, SelectedFiles};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    // From ashpd example
    let files = SelectedFiles::open_file()
        .title("Custom title")
        .accept_label("Custom label")
        .modal(true)
        .multiple(true)
        .choice(
            Choice::new("encoding", "Encoding", "latin15")
                .insert("utf8", "Unicode (UTF-8)")
                .insert("latin15", "Western"),
        )
        // A trick to have a checkbox
        .choice(Choice::boolean("re-encode", "Re-encode", false))
        .filter(FileFilter::new("Rust source").mimetype("text/rust"))
        .filter(FileFilter::new("TOML source").glob("*.toml"))
        .filter(FileFilter::new("SVG Image").mimetype("image/svg+xml"))
        .send()
        .await?
        .response()?;

    println!("{:#?}", files);

    Ok(())
}
