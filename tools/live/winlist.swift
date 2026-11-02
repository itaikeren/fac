// winlist.swift: list on-screen windows as "<window-id> <pid> <layer> <x,y,w,h> <owner> | <title>".
//
//   winlist --owner <substring>   windows whose owner name contains it (case-insensitive)
//   winlist --pid <pid>           windows owned by that process
//
// The window id is the CGWindowID that `screencapture -l` takes. Titles need
// Screen Recording permission; without it they print empty.
import CoreGraphics
import Foundation

let args = CommandLine.arguments
var owner: String?
var pid: Int?
if args.count == 3 && args[1] == "--owner" {
  owner = args[2].lowercased()
} else if args.count == 3 && args[1] == "--pid", let p = Int(args[2]) {
  pid = p
} else {
  FileHandle.standardError.write("usage: winlist --owner <name> | --pid <pid>\n".data(using: .utf8)!)
  exit(2)
}

let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
for w in list {
  let name = w[kCGWindowOwnerName as String] as? String ?? ""
  let wpid = w[kCGWindowOwnerPID as String] as? Int ?? -1
  if let o = owner, !name.lowercased().contains(o) { continue }
  if let p = pid, wpid != p { continue }
  let id = w[kCGWindowNumber as String] as? Int ?? -1
  let layer = w[kCGWindowLayer as String] as? Int ?? -1
  let b = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
  let bounds = ["X", "Y", "Width", "Height"].map { "\(Int(b[$0] as? Double ?? -1))" }.joined(separator: ",")
  let title = w[kCGWindowName as String] as? String ?? ""
  print("\(id) \(wpid) \(layer) \(bounds) \(name) | \(title)")
}
