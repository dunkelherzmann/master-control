// A small window with ordinary AppKit controls, for the macOS accessibility tests to read and act on. It opens without
// taking the keyboard or showing in the Dock, and closes when its input ends or after a minute.
import AppKit

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let window = NSWindow(contentRect: NSRect(x: 120, y: 320, width: 360, height: 220), styleMask: [.titled], backing: .buffered, defer: false)
window.title = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "mc test: tree"
final class Presses: NSObject {
    var count = 0
    @objc func pressed(_ sender: Any?) {
        count += 1
        label.stringValue = "pressed \(count)"
    }
}
let presses = Presses()
let button = NSButton(title: "Save it", target: presses, action: #selector(Presses.pressed(_:)))
button.frame = NSRect(x: 20, y: 160, width: 120, height: 32)
let field = NSTextField(string: "first text")
field.frame = NSRect(x: 20, y: 110, width: 220, height: 24)
let label = NSTextField(labelWithString: "A label")
label.frame = NSRect(x: 20, y: 70, width: 220, height: 20)
let box = NSButton(checkboxWithTitle: "Enable thing", target: nil, action: nil)
box.frame = NSRect(x: 20, y: 30, width: 220, height: 20)
box.state = .on
final class Choice: NSObject {
    @objc func chosen(_ sender: Any?) {}
}
let choice = Choice()
let one = NSButton(radioButtonWithTitle: "One", target: choice, action: #selector(Choice.chosen(_:)))
one.frame = NSRect(x: 250, y: 160, width: 90, height: 20)
one.state = .on
let two = NSButton(radioButtonWithTitle: "Two", target: choice, action: #selector(Choice.chosen(_:)))
two.frame = NSRect(x: 250, y: 135, width: 90, height: 20)
for view in [button, field, label, box, one, two] as [NSView] { window.contentView?.addSubview(view) }
// An Edit menu, as every real app has: keyboard shortcuts such as Select All are its items' key equivalents.
let menuBar = NSMenu()
let editItem = NSMenuItem()
menuBar.addItem(editItem)
let edit = NSMenu(title: "Edit")
edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
editItem.submenu = edit
app.mainMenu = menuBar
window.orderFrontRegardless()
print("ready")
fflush(stdout)
DispatchQueue.global().async {
    while readLine() != nil {}
    exit(0)
}
DispatchQueue.main.asyncAfter(deadline: .now() + 60) { exit(0) }
app.run()
