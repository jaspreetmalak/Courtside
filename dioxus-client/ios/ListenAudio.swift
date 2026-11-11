import AVFoundation
import Foundation

enum ListenTrack: UInt8, CaseIterable {
    case callee = 1
    case assistant = 2
    case user = 3
}

// Independent players are essential: inbound Twilio media includes silence
// throughout an assistant utterance. A single FIFO doubles the call's duration.
final class ListenAudio {
    let engine = AVAudioEngine()
    private let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 8000, channels: 1, interleaved: false)!
    private var players: [ListenTrack: AVAudioPlayerNode] = [:]
    private let lock = NSLock()
    private var queued: [ListenTrack: Int] = [:]
    private var generations: [ListenTrack: Int] = [:]

    init() {
        for track in ListenTrack.allCases {
            let player = AVAudioPlayerNode()
            players[track] = player
            engine.attach(player)
            engine.connect(player, to: engine.mainMixerNode, format: format)
        }
    }

    func start() throws {
        engine.prepare()
        try engine.start()
        for player in players.values { player.play() }
    }

    func play(_ packet: Data) {
        guard let first = packet.first, let track = ListenTrack(rawValue: first),
              let player = players[track], packet.count <= 240001 else { return }
        let audio = packet.dropFirst()
        if audio.isEmpty { clear(track); return }
        guard engine.isRunning,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(audio.count)),
              let samples = buffer.floatChannelData?[0] else { return }
        buffer.frameLength = AVAudioFrameCount(audio.count)
        for (index, byte) in audio.enumerated() { samples[index] = VoiceCodec.decode(byte) }

        // Caller/user media is produced in real time; catch up after a network
        // stall instead of replaying it late. Assistant audio arrives in bursts
        // ahead of PSTN playback, so retain the utterance on its own player.
        let limit = track == .assistant ? 240000 : 2400
        lock.lock()
        let reset = queued[track, default: 0] + audio.count > limit
        if reset {
            queued[track] = 0
            generations[track, default: 0] += 1
        }
        queued[track, default: 0] += audio.count
        let generation = generations[track, default: 0]
        lock.unlock()
        if reset { player.stop(); player.play() }
        player.scheduleBuffer(buffer, completionCallbackType: .dataPlayedBack) { [weak self] _ in
            guard let self = self else { return }
            self.lock.lock()
            if self.generations[track, default: 0] == generation {
                self.queued[track] = max(0, self.queued[track, default: 0] - audio.count)
            }
            self.lock.unlock()
        }
    }

    func clear(_ track: ListenTrack) {
        lock.lock()
        queued[track] = 0
        generations[track, default: 0] += 1
        lock.unlock()
        players[track]?.stop()
        if engine.isRunning { players[track]?.play() }
    }

    func stop() {
        for track in ListenTrack.allCases { clear(track) }
        engine.stop()
    }

    deinit { stop() }
}
