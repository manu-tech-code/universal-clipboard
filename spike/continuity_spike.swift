import AppKit
import Foundation

// Spike: does Apple's Universal Clipboard cooperate with a third-party process?
//   write <text>  -> put text on the Mac general pasteboard (what our app would do on receive)
//   watch         -> log every pasteboard change with its types (what our app would do on send)

let pb = NSPasteboard.general
let args = CommandLine.arguments
let stamp: () -> String = {
    let f = DateFormatter(); f.dateFormat = "HH:mm:ss.SSS"; return f.string(from: Date())
}

switch args.count > 1 ? args[1] : "" {
case "write":
    let text = args.dropFirst(2).joined(separator: " ")
    pb.clearContents()
    pb.setString(text, forType: .string)
    print("[\(stamp())] wrote to pasteboard (changeCount \(pb.changeCount)): \(text)")

case "watch":
    var last = pb.changeCount
    print("[\(stamp())] watching, changeCount \(last). Copy something on your iPhone now. Ctrl+C to stop.")
    setvbuf(stdout, nil, _IOLBF, 0)
    while true {
        let c = pb.changeCount
        if c != last {
            last = c
            let types = pb.types?.map { $0.rawValue } ?? []
            let text = pb.string(forType: .string) ?? "(no plain text)"
            print("[\(stamp())] CHANGE #\(c)")
            print("   types: \(types.joined(separator: ", "))")
            print("   text:  \(text.prefix(120))")
        }
        Thread.sleep(forTimeInterval: 0.1)
    }

default:
    print("usage: continuity_spike write <text> | watch")
}
