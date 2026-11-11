import Foundation
import AVFoundation

@main
struct VoiceCodecTests {
    static func main() {
        precondition(VoiceCodec.encode(0) == 0xff)
        precondition(VoiceCodec.decode(0xff) == 0)
        precondition(VoiceCodec.decode(0x7f) == 0)
        precondition(VoiceCodec.encode(1) == 0x80)
        precondition(VoiceCodec.encode(-1) == 0)
        for value in 0...255 {
            let byte = UInt8(value)
            let canonical = value == 0x7f ? UInt8(0xff) : byte
            precondition(VoiceCodec.encode(VoiceCodec.decode(byte)) == canonical)
        }
        for sample in -32768...32767 {
            let original = Float(sample) / 32768
            let restored = VoiceCodec.decode(VoiceCodec.encode(original))
            precondition(abs(original - restored) < 0.021)
        }
        print("G.711 mu-law: silence, sign, all 256 codes, and all 65,536 PCM samples passed.")
        for (rate, channels) in [(48000.0, 1), (48000.0, 2), (44100.0, 1), (16000.0, 1)] {
            let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: AVAudioChannelCount(channels), interleaved: false)!
            let encoder = VoiceEncoder(input: format)!
            let count = Int(rate * 0.02)
            var packets: [Data] = []
            for frame in 0..<100 {
                let input = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(count))!
                input.frameLength = AVAudioFrameCount(count)
                for channel in 0..<channels {
                    for sample in 0..<count {
                        input.floatChannelData![channel][sample] = Float(sin(2 * .pi * 440 * Double(frame * count + sample) / rate)) * 0.5
                    }
                }
                packets.append(contentsOf: encoder.encode(input))
            }
            precondition((98...100).contains(packets.count), "Resampler dropped audio: \(rate), \(channels), \(packets.count) frames")
            precondition(packets.allSatisfy { $0.count == 160 })
            let pcm = packets.flatMap { $0.map(VoiceCodec.decode) }
            let rms = sqrt(pcm.reduce(0) { $0 + $1 * $1 } / Float(pcm.count))
            precondition(rms > 0.30 && rms < 0.40)
            let crossings = zip(pcm, pcm.dropFirst()).filter { $0.0 <= 0 && $0.1 > 0 }.count
            precondition((850...900).contains(crossings), "Resampler changed voice timing")
        }
        print("Native microphone resampling: 16/44.1/48 kHz, mono/stereo, 20 ms frames, amplitude, and timing passed.")
    }
}
