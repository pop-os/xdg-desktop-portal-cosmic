allow = Allow
allow-once = Allow once
always-allow = Always allow
deny = Deny
cancel = Cancel
capture = Capture
share = Share
save-to = Save to
    .clipboard = { save-to } Clipboard
    .pictures = { save-to } Pictures
    .documents = { save-to } Documents
choose-folder = Choose folder

share-screen = Share your screen
    .description = The system wants to share the contents of your screen with "{$app_name}". Select a screen or window to share.
unknown-application = Unknown Application
output = Output
window = Window

# "keyboard" / "pointer" / "touchscreen" label the device icons
# "device-*" are the same devices as they read inside "description",
# joined by "device-list-pair" for the last two ("mouse and touchscreen")
# and "device-list-comma" for any earlier one ("keyboard, mouse and touchscreen").
remote-desktop = Allow remote control?
    .description = "{$app_name}" wants to remotely control this device. Allow this application to remotely control your {$devices}?
    .keyboard = Keyboard
    .pointer = Pointer
    .touchscreen = Touchscreen
    .device-keyboard = keyboard
    .device-pointer = mouse
    .device-touchscreen = touchscreen
    .device-list-fallback = input devices
    .device-list-pair = { $first } and { $second }
    .device-list-comma = { $first }, { $second }
    .select-screen-or-window = Select a screen or a window to share with "{$app_name}".
    .select-screen = Select a screen to share with "{$app_name}".
    .select-window = Select a window to share with "{$app_name}".
    .select-screens-or-windows = Select one or more screens or windows to share with "{$app_name}".
    .select-screens = Select one or more screens to share with "{$app_name}".
    .select-windows = Select one or more windows to share with "{$app_name}".
    .allow-while-running = Allow while running
