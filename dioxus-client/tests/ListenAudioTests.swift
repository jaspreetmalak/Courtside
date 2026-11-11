import AVFoundation
import Foundation

@main
struct ListenAudioTests {
    static func tone(_ frequency: Double, start: Int, count: Int) -> [UInt8] {
        (start..<(start + count)).map { VoiceCodec.encode(Float(sin(2 * .pi * frequency * Double($0) / 8000)) * 0.25) }
    }

    static func main() throws {
        let pipeline = ListenAudio()
        let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 8000, channels: 1, interleaved: false)!
        try pipeline.engine.enableManualRenderingMode(.offline, format: format, maximumFrameCount: 160)
        try pipeline.start()
        // Assistant delivers two seconds at once, while the caller sends
        // continuous 20 ms frames. Both must be audible in the same two seconds.
        pipeline.play(Data([ListenTrack.assistant.rawValue] + tone(1000, start: 0, count: 16000)))
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 160)!
        var pcm: [Float] = []
        for frame in 0..<100 {
            pipeline.play(Data([ListenTrack.callee.rawValue] + tone(440, start: frame * 160, count: 160)))
            let status = try pipeline.engine.renderOffline(160, to: buffer)
            precondition(status == .success)
            pcm.append(contentsOf: UnsafeBufferPointer(start: buffer.floatChannelData![0], count: Int(buffer.frameLength)))
        }
        func amplitude(_ frequency: Double) -> Double {
            let real = pcm.enumerated().reduce(0.0) { $0 + Double($1.element) * cos(2 * .pi * frequency * Double($1.offset) / 8000) }
            let imaginary = pcm.enumerated().reduce(0.0) { $0 + Double($1.element) * sin(2 * .pi * frequency * Double($1.offset) / 8000) }
            return 2 * hypot(real, imaginary) / Double(pcm.count)
        }
        precondition(pcm.count == 16000)
        precondition(amplitude(440) > 0.20, "Caller was delayed behind the assistant")
        precondition(amplitude(1000) > 0.20, "Assistant burst was discarded")
        pipeline.play(Data([ListenTrack.assistant.rawValue] + tone(1000, start: 0, count: 8000)))
        pipeline.play(Data([ListenTrack.assistant.rawValue]))
        pipeline.play(Data([ListenTrack.callee.rawValue]))
        for _ in 0..<5 {
            let status = try pipeline.engine.renderOffline(160, to: buffer)
            precondition(status == .success)
        }
        let remaining = UnsafeBufferPointer(start: buffer.floatChannelData![0], count: Int(buffer.frameLength))
        precondition(remaining.allSatisfy { abs($0) < 0.001 }, "Cleared speech was still played")
        // A network burst must not make real-time caller audio grow without
        // bound, or discard the independently buffered assistant utterance.
        pipeline.play(Data([ListenTrack.assistant.rawValue] + tone(1000, start: 0, count: 8000)))
        for _ in 0..<200 { pipeline.play(Data([ListenTrack.callee.rawValue] + [UInt8](repeating: 0xff, count: 160))) }
        var recovered: [Float] = []
        for _ in 0..<50 {
            let status = try pipeline.engine.renderOffline(160, to: buffer)
            precondition(status == .success)
            recovered.append(contentsOf: UnsafeBufferPointer(start: buffer.floatChannelData![0], count: Int(buffer.frameLength)))
        }
        let rms = sqrt(recovered.reduce(0) { $0 + $1 * $1 } / Float(recovered.count))
        precondition(rms > 0.15, "Caller catch-up reset the assistant's player")
        pipeline.stop()
        print("Native listening: simultaneous voices, assistant bursts, clearing, and caller backlog recovery passed.")
    }
}
