// Reads the QR code in a picture and prints its text: the reader of this
// machine (macOS), which is not the code that drew it.
//
// usage: swift tools/qr.swift PICTURE.png
import Foundation
import Vision

let url = URL(fileURLWithPath: CommandLine.arguments[1])
let request = VNDetectBarcodesRequest()
request.symbologies = [.qr]
do {
    try VNImageRequestHandler(url: url).perform([request])
} catch {
    FileHandle.standardError.write("cannot read \(url.path): \(error)\n".data(using: .utf8)!)
    exit(2)
}
guard let found = request.results?.first, let text = found.payloadStringValue else {
    FileHandle.standardError.write("no code found\n".data(using: .utf8)!)
    exit(1)
}
print(text, terminator: "")
