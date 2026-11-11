import AVFoundation
import CallKit
import Foundation

typealias VoiceCallback = @convention(c) (UnsafePointer<CChar>) -> Void

// CallKit owns session activation. The webview never captures microphone audio
// or carries media, so a joined call survives navigation and screen locking.
final class CallVoice: NSObject, CXProviderDelegate, URLSessionWebSocketDelegate {
    static let shared = CallVoice()
    private let provider: CXProvider
    private let providerConfiguration: CXProviderConfiguration
    private let controller = CXCallController()
    private var callID: String?
    private var callUUID: UUID?
    private var callback: VoiceCallback?
    private var endpoint: URL?
    private var bearer = ""
    private var peer = ""
    private var session: URLSession?
    private var socket: URLSessionWebSocketTask?
    private var audio: VoiceAudio?
    private var audioActivated = false
    private var socketOpened = false
    private var readySent = false
    private var connected = false
    private var incoming = false
    private var answering = false
    private var endingFromApp = false
    private var muted = false
    private var sending = false
    private var packets: [Data] = []
    private var watchdog: Timer?
    private var audioRetry: DispatchWorkItem?
    private var audioAttempts = 0

    var isActive: Bool { callID != nil && (!incoming || answering || connected) }

    private override init() {
        let config = CXProviderConfiguration()
        config.supportedHandleTypes = [.generic]
        config.maximumCallGroups = 1
        config.maximumCallsPerCallGroup = 1
        config.supportsVideo = false
        config.includesCallsInRecents = false
        _ = AVAudioSession.sharedInstance()
        providerConfiguration = config
        provider = CXProvider(configuration: config)
        super.init()
        provider.setDelegate(self, queue: .main)
        NotificationCenter.default.addObserver(
            self, selector: #selector(audioConfigurationChanged),
            name: .AVAudioEngineConfigurationChange, object: nil
        )
    }

    func start(url: String, token: String, id: String, peer: String, callback: @escaping VoiceCallback) {
        if callID == id, incoming, let uuid = callUUID {
            if answering || connected { return }
            controller.request(CXTransaction(action: CXAnswerCallAction(call: uuid))) { [weak self] error in
                DispatchQueue.main.async {
                    guard let self = self, self.callUUID == uuid, let error = error else { return }
                    self.finish(type: "error", message: audioFailure(error, activity: "answer the call"), reason: .failed)
                }
            }
            return
        }
        guard callID == nil else {
            emit(callback, id: id, type: "error", message: "You are already joined to a call.")
            return
        }
        guard let url = URL(string: url), url.scheme == "wss" else {
            emit(callback, id: id, type: "error", message: "A secure voice connection is required.")
            return
        }
        // Release playback's session before asking CallKit to activate a call.
        CallListen.shared.stop(id: "")
        self.callback = callback
        callID = id
        let uuid = UUID()
        callUUID = uuid
        endpoint = url
        bearer = token
        self.peer = peer
        emit("connecting", "Joining the call…")
        AVAudioSession.sharedInstance().requestRecordPermission { [weak self] allowed in
            DispatchQueue.main.async {
                guard let self = self, self.callUUID == uuid else { return }
                guard allowed else {
                    self.finish(type: "error", message: "Allow microphone access in iPhone Settings to join the call.", reason: .failed)
                    return
                }
                self.requestStart()
            }
        }
    }

    // Report an actual caller invitation to the system. No microphone, socket,
    // or assistant interruption occurs until the owner answers in CallKit.
    func invite(url: String, token: String, id: String, peer: String, seconds: Double, callback: @escaping VoiceCallback) {
        if callID == id { return }
        guard callID == nil else {
            emit(callback, id: id, type: "error", message: "Another call is already using Courtside audio.")
            return
        }
        guard let endpoint = URL(string: url), endpoint.scheme == "wss", seconds > 0 else { return }
        let uuid = UUID()
        self.callback = callback
        callID = id
        callUUID = uuid
        self.endpoint = endpoint
        bearer = token
        self.peer = peer
        incoming = true
        provider.configuration = providerConfiguration
        let update = CXCallUpdate()
        update.remoteHandle = CXHandle(type: .generic, value: peer.isEmpty ? "Courtside caller" : peer)
        update.localizedCallerName = peer.isEmpty ? "Caller via Courtside" : "\(peer) via Courtside"
        update.hasVideo = false
        update.supportsHolding = false
        update.supportsGrouping = false
        update.supportsUngrouping = false
        update.supportsDTMF = false
        provider.reportNewIncomingCall(with: uuid, update: update) { [weak self] error in
            DispatchQueue.main.async {
                guard let self = self else { return }
                guard self.callUUID == uuid else {
                    if error == nil { self.provider.reportCall(with: uuid, endedAt: Date(), reason: .remoteEnded) }
                    return
                }
                if let error = error {
                    self.finish(type: "error", message: audioFailure(error, activity: "show the incoming call"), reason: .failed)
                    return
                }
                // A fast answer can arrive before this completion callback.
                guard !self.answering else { return }
                self.emit("ringing", "The caller would like to speak with you.")
                self.watchdog?.invalidate()
                self.watchdog = Timer.scheduledTimer(withTimeInterval: min(seconds, 45), repeats: false) { [weak self] _ in
                    guard let self = self, self.callUUID == uuid, !self.answering else { return }
                    self.finish(type: "declined", message: "Courtside will take a message.", reason: .unanswered)
                }
            }
        }
    }

    private func requestStart() {
        guard let uuid = callUUID else { return }
        // Refresh the audio session ID before every call. Apple recommends
        // this for providers created during iPhone unlock/initialization.
        provider.configuration = providerConfiguration
        let handle = CXHandle(type: .generic, value: peer.isEmpty ? "Courtside call" : peer)
        let action = CXStartCallAction(call: uuid, handle: handle)
        action.isVideo = false
        watchdog?.invalidate()
        watchdog = Timer.scheduledTimer(withTimeInterval: 30, repeats: false) { [weak self] _ in
            guard let self = self, self.callUUID == uuid, !self.connected else { return }
            self.finish(type: "error", message: "Couldn’t start the join. Courtside still has the call.", reason: .failed)
        }
        // An outgoing CallKit action joins immediately after system audio
        // activation, without presenting an incoming invitation to answer.
        controller.request(CXTransaction(action: action)) { [weak self] error in
            DispatchQueue.main.async {
                guard let self = self else { return }
                guard self.callUUID == uuid else {
                    if error == nil { self.provider.reportCall(with: uuid, endedAt: Date(), reason: .remoteEnded) }
                    return
                }
                if let error = error as NSError? {
                    NSLog("Courtside CallKit start failed: %@ (%ld)", error.domain, error.code)
                    self.finish(type: "error", message: "iPhone couldn’t start the join (\(error.domain), code \(error.code)).", reason: .failed)
                } else {
                    NSLog("Courtside CallKit start requested")
                }
            }
        }
    }

    func end(id: String) {
        guard id.isEmpty || callID == id else { return }
        guard let uuid = callUUID else { return }
        endingFromApp = true
        controller.request(CXTransaction(action: CXEndCallAction(call: uuid))) { [weak self] error in
            DispatchQueue.main.async {
                guard let self = self, self.callUUID == uuid, error != nil else { return }
                self.finish(type: "ended", message: "Returning the call to Courtside…", reason: .remoteEnded)
            }
        }
    }

    func cancelInvitation(id: String) {
        guard callID == id, incoming, !answering, !connected else { return }
        finish(type: "ended", message: "Courtside is handling the caller.", reason: .remoteEnded)
    }

    func provider(_ provider: CXProvider, perform action: CXStartCallAction) {
        guard action.callUUID == callUUID, endpoint != nil else { action.fail(); return }
        do {
            let audioSession = AVAudioSession.sharedInstance()
            try audioSession.setCategory(.playAndRecord, mode: .voiceChat, options: [.allowBluetoothHFP])
            try audioSession.setPreferredSampleRate(48000)
            try audioSession.setPreferredIOBufferDuration(0.02)
        } catch {
            action.fail()
            finish(type: "error", message: audioFailure(error, activity: "configure call audio"), reason: .failed)
            return
        }
        NSLog("Courtside CallKit direct join starting")
        let uuid = action.callUUID
        watchdog?.invalidate()
        watchdog = Timer.scheduledTimer(withTimeInterval: 30, repeats: false) { [weak self] _ in
            guard let self = self, self.callUUID == uuid, !self.connected else { return }
            self.finish(type: "error", message: "Call audio didn’t activate. Courtside still has the call.", reason: .failed)
        }
        // Activation belongs to CallKit. Wait for didActivate before building
        // the microphone graph or opening the media connection.
        action.fulfill()
        let update = CXCallUpdate()
        update.remoteHandle = action.handle
        update.localizedCallerName = peer.isEmpty ? "Courtside call" : peer
        update.hasVideo = false
        update.supportsHolding = false
        update.supportsGrouping = false
        update.supportsUngrouping = false
        update.supportsDTMF = false
        provider.reportCall(with: uuid, updated: update)
        provider.reportOutgoingCall(with: uuid, startedConnectingAt: Date())
    }

    func provider(_ provider: CXProvider, perform action: CXAnswerCallAction) {
        guard action.callUUID == callUUID, incoming, !answering, endpoint != nil else { action.fail(); return }
        answering = true
        watchdog?.invalidate()
        emit("connecting", "Joining the caller…")
        let uuid = action.callUUID
        AVAudioSession.sharedInstance().requestRecordPermission { [weak self] allowed in
            DispatchQueue.main.async {
                guard let self = self, self.callUUID == uuid else { action.fail(); return }
                guard allowed else {
                    action.fail()
                    self.finish(type: "error", message: "Allow microphone access in iPhone Settings to join the caller.", reason: .failed)
                    return
                }
                CallListen.shared.stop(id: "")
                do {
                    let session = AVAudioSession.sharedInstance()
                    try session.setCategory(.playAndRecord, mode: .voiceChat, options: [.allowBluetoothHFP])
                    try session.setPreferredSampleRate(48000)
                    try session.setPreferredIOBufferDuration(0.02)
                } catch {
                    action.fail()
                    self.finish(type: "error", message: audioFailure(error, activity: "configure call audio"), reason: .failed)
                    return
                }
                self.watchdog = Timer.scheduledTimer(withTimeInterval: 30, repeats: false) { [weak self] _ in
                    guard let self = self, self.callUUID == uuid, !self.connected else { return }
                    self.finish(type: "error", message: "Call audio didn't activate. Courtside still has the caller.", reason: .failed)
                }
                // CallKit activates audio after this answer is fulfilled. The
                // server receives ready only once the native graph is running.
                action.fulfill()
            }
        }
    }

    private func openConnection() {
        guard socket == nil, let endpoint = endpoint, audio?.running == true else { return }
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 30
        config.waitsForConnectivity = false
        session = URLSession(configuration: config, delegate: self, delegateQueue: .main)
        var request = URLRequest(url: endpoint)
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        bearer = ""
        socket = session?.webSocketTask(with: request)
        socket?.maximumMessageSize = 65536
        socket?.resume()
        receive()
    }

    func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
        guard callID != nil else { return }
        guard !incoming || answering else { return }
        NSLog("Courtside CallKit audio activated")
        audioActivated = true
        audioAttempts = 0
        startAudio()
    }

    private func startAudio() {
        guard audioActivated, callID != nil else { return }
        audioRetry?.cancel()
        audioRetry = nil
        audio?.stop()
        audio = nil
        let uuid = callUUID
        do {
            let pipeline = try VoiceAudio { [weak self] packet in
                DispatchQueue.main.async {
                    guard let self = self, self.callUUID == uuid, self.connected else { return }
                    self.enqueue(self.muted ? Data(repeating: 0xff, count: 160) : packet)
                }
            }
            audio = pipeline
            try pipeline.start()
            NSLog("Courtside microphone engine running")
            audioAttempts = 0
            openConnection()
            sendReady()
        } catch {
            let error = error as NSError
            if error.domain == "CourtsideMicrophone", audioAttempts < 20 {
                audioAttempts += 1
                let retry = DispatchWorkItem { [weak self] in
                    guard let self = self, self.callUUID == uuid, self.audioActivated else { return }
                    self.startAudio()
                }
                audioRetry = retry
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1, execute: retry)
            } else {
                finish(type: "error", message: audioFailure(error, activity: "access call audio"), reason: .failed)
            }
        }
    }

    @objc private func audioConfigurationChanged(_ notification: Notification) {
        guard let engine = notification.object as? AVAudioEngine else { return }
        DispatchQueue.main.async { [weak self] in
            guard let self = self, self.audio?.engine === engine, !engine.isRunning,
                  self.audioActivated, self.callID != nil else { return }
            self.startAudio()
        }
    }

    func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
        audioRetry?.cancel()
        audioRetry = nil
        audioActivated = false
        audio?.stop()
        audio = nil
    }

    func provider(_ provider: CXProvider, perform action: CXSetMutedCallAction) {
        guard action.callUUID == callUUID else { action.fail(); return }
        muted = action.isMuted
        action.fulfill()
    }

    func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
        guard action.callUUID == callUUID else { action.fail(); return }
        let declined = incoming && !connected && !endingFromApp
        finish(type: declined ? "declined" : "ended", message: declined ? "Courtside will take a message." : "Returning the call to Courtside…", reason: nil)
        action.fulfill()
    }

    func providerDidReset(_ provider: CXProvider) {
        finish(type: "ended", message: "Returning the call to Courtside…", reason: nil)
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didOpenWithProtocol protocol: String?) {
        guard webSocketTask === socket else { return }
        socketOpened = true
        sendReady()
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didCloseWith closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        guard webSocketTask === socket else { return }
        finish(type: "ended", message: "The voice connection ended. Returning to Courtside…", reason: .remoteEnded)
    }

    private func sendReady() {
        guard socketOpened, audio?.running == true, !readySent else { return }
        readySent = true
        guard let socket = socket else { return }
        socket.send(.string("{\"type\":\"ready\"}")) { [weak self, weak socket] error in
            DispatchQueue.main.async {
                guard let self = self, let socket = socket, self.socket === socket, error != nil else { return }
                self.networkFailed()
            }
        }
    }

    private func receive() {
        guard let socket = socket else { return }
        socket.receive { [weak self, weak socket] result in
            DispatchQueue.main.async {
                guard let self = self, let socket = socket, self.socket === socket else { return }
                switch result {
                case .failure:
                    self.networkFailed()
                case .success(let message):
                    switch message {
                    case .data(let bytes):
                        if self.connected { self.audio?.play(bytes) }
                    case .string(let text):
                        if let data = text.data(using: .utf8),
                           let event = (try? JSONSerialization.jsonObject(with: data)) as? [String: String] {
                            switch event["type"] {
                            case "connected":
                                guard !self.connected else { break }
                                self.connected = true
                                self.watchdog?.invalidate()
                                self.watchdog = nil
                                if let uuid = self.callUUID, !self.incoming {
                                    self.provider.reportOutgoingCall(with: uuid, connectedAt: Date())
                                }
                                self.emit("connected", "You’re on the call. Use Return to AI when you’re done.")
                            case "error":
                                self.finish(type: "error", message: event["message"] ?? "Unable to join the call.", reason: .failed)
                            case "ended":
                                self.finish(type: "ended", message: event["message"] ?? "The call ended.", reason: .remoteEnded)
                            default: break
                            }
                        }
                    @unknown default: break
                    }
                    self.receive()
                }
            }
        }
    }

    private func enqueue(_ bytes: Data) {
        // Drop old microphone frames under network pressure rather than
        // delivering speech seconds late after the connection recovers.
        if packets.count >= 25 { packets.removeFirst(packets.count - 24) }
        packets.append(bytes)
        sendNext()
    }

    private func sendNext() {
        guard !sending, !packets.isEmpty, let socket = socket else { return }
        sending = true
        let packet = packets.removeFirst()
        socket.send(.data(packet)) { [weak self, weak socket] error in
            DispatchQueue.main.async {
                guard let self = self, let socket = socket, self.socket === socket else { return }
                self.sending = false
                if error != nil { self.networkFailed() } else { self.sendNext() }
            }
        }
    }

    private func networkFailed() {
        finish(type: "error", message: "Voice connection lost. Courtside will resume the call.", reason: .failed)
    }

    private func finish(type: String, message: String, reason: CXCallEndedReason?) {
        guard let id = callID else { return }
        let uuid = callUUID
        let callback = self.callback
        callID = nil
        callUUID = nil
        self.callback = nil
        watchdog?.invalidate()
        watchdog = nil
        audioRetry?.cancel()
        audioRetry = nil
        audioAttempts = 0
        let closingSocket = socket
        socket = nil
        closingSocket?.cancel(with: .normalClosure, reason: nil)
        session?.invalidateAndCancel()
        session = nil
        audio?.stop()
        audio = nil
        audioActivated = false
        socketOpened = false
        readySent = false
        connected = false
        incoming = false
        answering = false
        endingFromApp = false
        muted = false
        sending = false
        packets.removeAll()
        bearer = ""
        endpoint = nil
        if let uuid = uuid, let reason = reason {
            provider.reportCall(with: uuid, endedAt: Date(), reason: reason)
        }
        if let callback = callback { emit(callback, id: id, type: type, message: message) }
    }

    private func emit(_ type: String, _ message: String) {
        guard let callback = callback, let id = callID else { return }
        emit(callback, id: id, type: type, message: message)
    }

    private func emit(_ callback: VoiceCallback, id: String, type: String, message: String) {
        guard let data = try? JSONSerialization.data(withJSONObject: ["call_id": id, "type": type, "message": message]),
              let json = String(data: data, encoding: .utf8) else { return }
        json.withCString { callback($0) }
    }
}

private final class VoiceAudio {
    let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private let captureMixer = AVAudioMixerNode()
    private let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 8000, channels: 1, interleaved: false)!
    private var tapped = false
    private(set) var running = false
    private let playbackLock = NSLock()
    private var scheduled = 0
    private var playbackGeneration = 0

    init(packet: @escaping (Data) -> Void) throws {
        // Build the output side before switching to VoiceProcessingIO. Reading
        // an unconnected input node immediately after that switch can yield a
        // zero-channel format, which cannot initialize AVAudioConverter.
        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: format)
        let input = engine.inputNode
        try input.setVoiceProcessingEnabled(true)
        let hardware = input.outputFormat(forBus: 0)
        let session = AVAudioSession.sharedInstance()
        let rate = hardware.sampleRate > 0 ? hardware.sampleRate : session.sampleRate
        let channels = hardware.channelCount > 0 ? hardware.channelCount : AVAudioChannelCount(session.inputNumberOfChannels)
        NSLog("Courtside microphone format: node %.0f Hz/%u channels, session %.0f Hz/%ld channels", hardware.sampleRate, hardware.channelCount, session.sampleRate, session.inputNumberOfChannels)
        guard rate > 0, channels > 0,
              let captureFormat = AVAudioFormat(standardFormatWithSampleRate: rate, channels: channels) else {
            throw NSError(domain: "CourtsideMicrophone", code: 10, userInfo: [NSLocalizedDescriptionKey: "The microphone format is not ready."])
        }
        engine.attach(captureMixer)
        engine.connect(input, to: captureMixer, format: captureFormat)
        engine.connect(captureMixer, to: engine.mainMixerNode, format: captureFormat)
        captureMixer.outputVolume = 0 // Enable capture without monitoring your mic.
        guard let encoder = VoiceEncoder(input: captureFormat) else {
            throw NSError(domain: "CourtsideMicrophone", code: 11, userInfo: [NSLocalizedDescriptionKey: "The microphone format could not be converted."])
        }
        input.installTap(onBus: 0, bufferSize: 960, format: captureFormat) { buffer, _ in
            for frame in encoder.encode(buffer) { packet(frame) }
        }
        tapped = true
    }

    func start() throws {
        engine.prepare()
        try engine.start()
        player.play()
        running = true
    }

    func play(_ bytes: Data) {
        guard running, !bytes.isEmpty, bytes.count <= 1600,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(bytes.count)),
              let samples = buffer.floatChannelData?[0] else { return }
        buffer.frameLength = AVAudioFrameCount(bytes.count)
        for (i, byte) in bytes.enumerated() { samples[i] = VoiceCodec.decode(byte) }
        playbackLock.lock()
        let reset = scheduled >= 25
        if reset { scheduled = 0; playbackGeneration += 1 }
        scheduled += 1
        let generation = playbackGeneration
        playbackLock.unlock()
        if reset { player.stop(); player.play() }
        player.scheduleBuffer(buffer, completionCallbackType: .dataPlayedBack) { [weak self] _ in
            guard let self = self else { return }
            self.playbackLock.lock()
            if self.playbackGeneration == generation { self.scheduled = max(0, self.scheduled - 1) }
            self.playbackLock.unlock()
        }
    }

    func stop() {
        running = false
        if tapped { engine.inputNode.removeTap(onBus: 0); tapped = false }
        player.stop()
        engine.stop()
    }

    deinit { stop() }
}

@_cdecl("courtside_voice_start")
func courtsideVoiceStart(_ url: UnsafePointer<CChar>, _ token: UnsafePointer<CChar>, _ id: UnsafePointer<CChar>, _ peer: UnsafePointer<CChar>, _ callback: @escaping VoiceCallback) {
    let endpoint = String(cString: url)
    let bearer = String(cString: token)
    let callID = String(cString: id)
    let name = String(cString: peer)
    DispatchQueue.main.async {
        CallVoice.shared.start(url: endpoint, token: bearer, id: callID, peer: name, callback: callback)
    }
}

@_cdecl("courtside_voice_end")
func courtsideVoiceEnd(_ id: UnsafePointer<CChar>) {
    let callID = String(cString: id)
    DispatchQueue.main.async { CallVoice.shared.end(id: callID) }
}

@_cdecl("courtside_voice_cancel_invitation")
func courtsideVoiceCancelInvitation(_ id: UnsafePointer<CChar>) {
    let callID = String(cString: id)
    DispatchQueue.main.async { CallVoice.shared.cancelInvitation(id: callID) }
}

@_cdecl("courtside_voice_invite")
func courtsideVoiceInvite(_ url: UnsafePointer<CChar>, _ token: UnsafePointer<CChar>, _ id: UnsafePointer<CChar>, _ peer: UnsafePointer<CChar>, _ seconds: Double, _ callback: @escaping VoiceCallback) {
    let endpoint = String(cString: url)
    let bearer = String(cString: token)
    let callID = String(cString: id)
    let name = String(cString: peer)
    DispatchQueue.main.async {
        CallVoice.shared.invite(url: endpoint, token: bearer, id: callID, peer: name, seconds: seconds, callback: callback)
    }
}
