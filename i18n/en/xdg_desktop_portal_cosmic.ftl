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

print = Print
save = Save
close = Close
not-supported = Not supported

destination = Destination
no-printers-found = No printers found
no-printer-selected = No printer selected
printer-status = { $name } - { $state }
printer-state-idle = Idle
printer-state-processing = Processing
printer-state-stopped = Stopped
save-as-pdf = Save as PDF
save-output-to-pdf = Save output to PDF file
pdf-document = PDF Document
save-pdf-document = Save PDF Document

preset = Preset
presets = Presets
save-preset = Save preset
save-preset-action = Save current settings as preset...
edit-presets-action = Edit preset list...
add-preset = Add preset
preset-name = Preset name
preset-default-name = Preset { $timestamp }
preset-builtin-default = Default
preset-builtin-color = Color
preset-builtin-bw = Black and White

pages = Pages
page-set-all = All pages
page-set-current = Current page
page-set-odd = Odd pages only
page-set-even = Even pages only
page-set-custom = Custom range
page-set-custom-value = Custom, { $range }
page-range-placeholder = e.g. 1-5, 8, 11-13
page-range-invalid = Invalid format: use numbers and ranges (e.g. 1-5, 8)

copies = Copies
collate = Collate
paper-size = Paper size
print-on-sides = Print on sides
color-mode-color = Color
color-mode-monochrome = Greyscale
orientation-portrait = Portrait
orientation-landscape = Landscape

layout = Layout
pages-per-sheet = Pages per sheet
layout-direction = Layout direction
margins = Margins
margins-default = Default
margins-none = None
margins-minimum = Minimum
margins-custom = Custom
margin-vertical = Top & bottom margin
margin-horizontal = Left & right margin
margin-value = { $mm } mm
border = Border
border-none = None
border-single = Single
border-double = Double
scaling = Scaling
scaling-auto = Auto
scaling-auto-fit = Auto fit
scaling-fit = Fit to page
scaling-fill = Fill page
scaling-custom = Custom
print-header-footer = Print header and footer
print-background = Print background

paper-handling = Paper handling & quality
reverse-order = Print pages in reverse order
paper-tray = Paper tray
paper-type = Paper type
print-quality = Print quality

sides-one-sided = One-sided
sides-two-sided-long-edge = Two-sided (long edge)
sides-two-sided-short-edge = Two-sided (short edge)

media-source-auto = Auto select
media-source-main = Main tray
media-source-manual = Manual feed
media-source-by-pass-tray = Bypass tray
media-source-top = Top tray
media-source-middle = Middle tray
media-source-bottom = Bottom tray
media-source-envelope = Envelope feeder
media-source-large-capacity = Large capacity tray

media-type-auto = Automatic
media-type-stationery = Plain paper
media-type-stationery-letterhead = Letterhead
media-type-stationery-lightweight = Lightweight paper
media-type-stationery-heavyweight = Heavyweight paper
media-type-cardstock = Cardstock
media-type-labels = Labels
media-type-envelope = Envelope
media-type-transparency = Transparency
media-type-recycled = Recycled paper
media-type-photographic = Photo paper
media-type-photographic-glossy = Glossy photo paper
media-type-photographic-matte = Matte photo paper

print-quality-draft = Draft
print-quality-normal = Normal
print-quality-high = High
