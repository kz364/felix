// Felix Draft: an input method that shows Felix's rough live transcript as
// marked text (underlined, not yet final) in whatever field has focus, and
// clears it before Felix pastes the real text. It never handles keys; every
// keystroke passes straight through to the app.
//
// Felix talks to it over a unix socket, one line per message:
//   "D <text>"  show <text> as the draft (replaces the previous draft)
//   "C"         clear the draft; answers "ok" once it's gone
//   "P"         ping; answers "ok 1" if a text field is attached, "ok 0" if not

import Cocoa
import InputMethodKit

let socketPath = NSString(string: "~/Library/Application Support/com.pais.handy/draft-ime.sock")
    .expandingTildeInPath

final class Draft {
    static let shared = Draft()
    /// The client of the most recently activated controller.
    weak var client: AnyObject?
    private var showing = false

    private var textInput: IMKTextInput? { client as? IMKTextInput }

    func show(_ text: String) {
        guard let input = textInput else { return }
        let attributed = NSAttributedString(string: text, attributes: [
            .underlineStyle: NSUnderlineStyle.single.rawValue,
            .foregroundColor: NSColor.secondaryLabelColor,
        ])
        input.setMarkedText(
            attributed,
            selectionRange: NSRange(location: (text as NSString).length, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0))
        showing = true
    }

    func clear() {
        guard showing, let input = textInput else { showing = false; return }
        input.setMarkedText(
            "", selectionRange: NSRange(location: 0, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0))
        input.insertText("", replacementRange: NSRange(location: NSNotFound, length: 0))
        showing = false
    }

    func onMain<T>(_ work: () -> T) -> T {
        Thread.isMainThread ? work() : DispatchQueue.main.sync(execute: work)
    }

    // MARK: socket

    func listen() {
        Thread.detachNewThread { self.serve() }
    }

    private func serve() {
        unlink(socketPath)
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(socketPath.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { return }
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            raw.copyBytes(from: bytes)
            raw[bytes.count] = 0
        }
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bound == 0 else { return }
        chmod(socketPath, 0o600)
        Darwin.listen(fd, 4)
        while true {
            let conn = accept(fd, nil, nil)
            if conn < 0 { continue }
            Thread.detachNewThread { self.handle(conn) }
        }
    }

    private func handle(_ conn: Int32) {
        defer {
            onMain { self.clear() }  // a lost connection never leaves a draft behind
            close(conn)
        }
        var pending = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)
        while true {
            let n = read(conn, &buffer, buffer.count)
            if n <= 0 { return }
            pending.append(buffer, count: n)
            while let newline = pending.firstIndex(of: 0x0A) {
                let line = String(decoding: pending[pending.startIndex..<newline], as: UTF8.self)
                pending.removeSubrange(pending.startIndex...newline)
                respond(to: line, on: conn)
            }
        }
    }

    private func respond(to line: String, on conn: Int32) {
        var reply: String?
        if line.hasPrefix("D ") {
            let text = String(line.dropFirst(2))
            onMain { self.show(text) }
        } else if line == "C" {
            onMain { self.clear() }
            reply = "ok"
        } else if line == "P" {
            reply = onMain { self.textInput == nil ? "ok 0" : "ok 1" }
        }
        if let reply = reply {
            let data = Array((reply + "\n").utf8)
            _ = data.withUnsafeBufferPointer { write(conn, $0.baseAddress, $0.count) }
        }
    }
}

@objc(DraftController)
final class DraftController: IMKInputController {
    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool { false }

    override func activateServer(_ sender: Any!) {
        super.activateServer(sender)
        Draft.shared.client = sender as AnyObject?
    }

    override func deactivateServer(_ sender: Any!) {
        if Draft.shared.client === (sender as AnyObject?) {
            Draft.shared.clear()
        }
        super.deactivateServer(sender)
    }

    /// The app wants the composition ended (a click, a focus change): drop the
    /// draft rather than leaving it as typed text.
    override func commitComposition(_ sender: Any!) {
        if Draft.shared.client === (sender as AnyObject?) {
            Draft.shared.clear()
        }
    }
}

let connection = Bundle.main.infoDictionary?["InputMethodConnectionName"] as? String
    ?? "com.pais.inputmethod.FelixDraft_Connection"
let server = IMKServer(name: connection, bundleIdentifier: Bundle.main.bundleIdentifier)
Draft.shared.listen()
NSApplication.shared.run()
_ = server
