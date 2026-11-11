import Foundation
import AVFoundation

// Twilio's stream uses raw G.711 mu-law, mono, 8 kHz (160 bytes per 20 ms).
enum VoiceCodec {
    static func encode(_ sample: Float) -> UInt8 {
        var value = Int(max(-1, min(1, sample)) * 32767)
        let sign = value < 0 ? 0x80 : 0
        if value < 0 { value = -value }
        value = min(value, 32635) + 132
        var exponent = 7
        var mask = 0x4000
        while exponent > 0 && value & mask == 0 {
            exponent -= 1
            mask >>= 1
        }
        let mantissa = (value >> (exponent + 3)) & 0x0f
        return UInt8(~(sign | (exponent << 4) | mantissa) & 0xff)
    }

    static func decode(_ byte: UInt8) -> Float {
        let value = Int(~byte)
        let magnitude = (((value & 0x0f) << 3) + 132) << ((value >> 4) & 7)
        let sample = magnitude - 132
        return Float(value & 0x80 == 0 ? sample : -sample) / 32768
    }
}

// Hardware microphones use a variety of rates and channel layouts. Resample
// before packetizing, retaining partial frames across tap callbacks.
final class VoiceEncoder {
    private let converter: AVAudioConverter
    private let format: AVAudioFormat
    private let rate: Double
    private var bytes: [UInt8] = []

    init?(input: AVAudioFormat) {
        guard input.sampleRate > 0,
              let output = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 8000, channels: 1, interleaved: false),
              let converter = AVAudioConverter(from: input, to: output) else { return nil }
        self.converter = converter
        format = output
        rate = input.sampleRate
    }

    func encode(_ buffer: AVAudioPCMBuffer) -> [Data] {
        let capacity = AVAudioFrameCount(ceil(Double(buffer.frameLength) * 8000 / rate)) + 8
        guard let output = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else { return [] }
        var consumed = false
        var error: NSError?
        let status = converter.convert(to: output, error: &error) { _, state in
            if consumed { state.pointee = .noDataNow; return nil }
            consumed = true
            state.pointee = .haveData
            return buffer
        }
        guard status != .error, let samples = output.floatChannelData?[0] else { return [] }
        for i in 0..<Int(output.frameLength) { bytes.append(VoiceCodec.encode(samples[i])) }
        var packets: [Data] = []
        while bytes.count >= 160 {
            packets.append(Data(bytes.prefix(160)))
            bytes.removeFirst(160)
        }
        return packets
    }
}
