use ashpd::desktop::background::Background;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ashpd::Result<()> {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "FakeTestApp".into());

    // Based off of the ashpd docs
    // https://docs.rs/ashpd/latest/ashpd/desktop/background/index.html
    let response = Background::request()
        .reason("Testing the background portal")
        .auto_start(false)
        .dbus_activatable(false)
        .command(&[command])
        .send()
        .await?
        .response()?;

    assert!(!response.auto_start(), "Auto start should be disabled");
    assert!(
        response.run_in_background(),
        "App should have background permissions"
    );

    Ok(())
}
