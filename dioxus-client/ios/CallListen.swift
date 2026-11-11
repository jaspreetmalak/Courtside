import AVFoundation
import Foundation

func audioFailure(_ error: Error, activity: String) -> String {
    let error = error as NSError
    NSLog("Courtside audio failed to %@: %@ (%ld)", activity, error.domain, error.code)
    if error.domain == "CourtsideMicrophone" {
        return "The microphone couldn’t initialize. Try joining again (audio setup code \(error.code))."
    }
    if error.domain == NSOSStatusErrorDomain || error.domain == "AVAudioSessionErrorDomain",
       error.code == AVAudioSession.ErrorCode.insufficientPriority.rawValue || error.code == AVAudioSession.ErrorCode.cannotInterruptOthers.rawValue {
        return "iPhone audio is in use by another call. Finish that call, then try again."
    }
    return "iPhone couldn’t \(activity) (iOS error \(error.code)). Please try again."
}

// Keep incoming media off the webview/UI thread, including during keyboard
// transitions. Listening never records audio or asks for microphone access.
final class CallListen: NSObject, URLSessionWebSocketDelegate {
    static let shared = CallListen()
    private let queue = DispatchQueue(label: "com.courtside.listen", qos: .userInitiated)
    private var callID: String?
    private var callback: VoiceCallback?
    private var session: URLSession?
    private var socket: URLSessionWebSocketTask?
    private var audio: ListenAudio?
    private var ownsAudioSession = false
    private var connected = false
    private var watchdog: DispatchWorkItem?
    private var heartbeat: DispatchSourceTimer?

    private override init() {
        super.init()
        NotificationCenter.default.addObserver(self, selector: #selector(configurationChanged), name: .AVAudioEngineConfigurationChange, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(interrupted), name: AVAudioSession.interruptionNotification, object: nil)
    }

    func start(url: String, token: String, id: String, callback: @escaping VoiceCallback) {
        queue.async { [self] in
            self.finish(type: "ended", message: "")
            self.callID = id
            self.callback = callback
            guard let endpoint = URL(string: url), endpoint.scheme == "wss" else {
                self.finish(type: "error", message: "A secure live audio connection is required.")
                return
            }
            self.emit("connecting", "Connecting live audio…")
            do { try self.startAudio() } catch {
                self.finish(type: "error", message: audioFailure(error, activity: "start live listening"))
                return
            }
            let config = URLSessionConfiguration.default
            config.timeoutIntervalForRequest = 20
            config.waitsForConnectivity = false
            let delegateQueue = OperationQueue()
            delegateQueue.maxConcurrentOperationCount = 1
            delegateQueue.underlyingQueue = self.queue
            self.session = URLSession(configuration: config, delegate: self, delegateQueue: delegateQueue)
            var request = URLRequest(url: endpoint)
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            let socket = self.session!.webSocketTask(with: request)
            self.socket = socket
            socket.maximumMessageSize = 524288
            socket.resume()
            self.receive()
            let watchdog = DispatchWorkItem { [weak self, weak socket] in
                guard let self = self, let socket = socket, self.socket === socket, !self.connected else { return }
                self.finish(type: "error", message: "Live audio couldn’t connect. Tap Live listen to retry.")
            }
            self.watchdog = watchdog
            self.queue.asyncAfter(deadline: .now() + 20, execute: watchdog)
        }
    }

    // Synchronous session release ensures a following microphone start cannot
    // race playback's setActive(false). This is called outside our audio queue.
    func stop(id: String) {
        queue.sync {
            guard id.isEmpty || self.callID == id else { return }
            self.finish(type: "ended", message: "")
        }
    }

    private func startAudio() throws {
        audio?.stop()
        audio = nil
        let audioSession = AVAudioSession.sharedInstance()
        // Playback is intentional user-requested audio, and remains audible
        // with the iPhone Ring/Silent switch off and while the screen locks.
        try audioSession.setCategory(.playback, mode: .spokenAudio)
        try audioSession.setPreferredIOBufferDuration(0.02)
        try audioSession.setActive(true)
        ownsAudioSession = true
        let pipeline = ListenAudio()
        try pipeline.start()
        audio = pipeline
    }

    private func receive() {
        guard let socket = socket else { return }
        socket.receive { [weak self, weak socket] result in
            guard let self = self else { return }
            self.queue.async {
                guard let socket = socket, self.socket === socket else { return }
                switch result {
                case .failure:
                    self.finish(type: "error", message: "Live audio disconnected. Tap Live listen to reconnect.")
                case .success(let message):
                    switch message {
                    case .data(let packet):
                        if self.connected { self.audio?.play(packet) }
                    case .string(let text):
                        if let data = text.data(using: .utf8),
                           let event = (try? JSONSerialization.jsonObject(with: data)) as? [String: String] {
                            switch event["type"] {
                            case "connected":
                                self.connected = true
                                self.watchdog?.cancel()
                                self.watchdog = nil
                                self.emit("connected", "Listening live")
                                self.startHeartbeat(socket)
                            case "ended", "error":
                                self.finish(type: event["type"]!, message: event["message"] ?? "Live audio ended.")
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

    private func startHeartbeat(_ socket: URLSessionWebSocketTask) {
        heartbeat?.cancel()
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 15, repeating: 15)
        timer.setEventHandler { [weak self, weak socket] in
            guard let self = self, let socket = socket, self.socket === socket else { return }
            socket.sendPing { error in
                if error != nil {
                    self.queue.async {
                        guard self.socket === socket else { return }
                        self.finish(type: "error", message: "Live audio disconnected. Tap Live listen to reconnect.")
                    }
                }
            }
        }
        heartbeat = timer
        timer.resume()
    }

    @objc private func configurationChanged(_ notification: Notification) {
        guard let engine = notification.object as? AVAudioEngine else { return }
        queue.async {
            guard self.audio?.engine === engine, !engine.isRunning, self.callID != nil else { return }
            do { try self.startAudio() } catch {
                self.finish(type: "error", message: audioFailure(error, activity: "restore live audio"))
            }
        }
    }

    @objc private func interrupted(_ notification: Notification) {
        let info = notification.userInfo
        let type = (info?[AVAudioSessionInterruptionTypeKey] as? NSNumber)?.uintValue
        let options = (info?[AVAudioSessionInterruptionOptionKey] as? NSNumber)?.uintValue ?? 0
        queue.async {
            guard self.callID != nil else { return }
            if type == AVAudioSession.InterruptionType.began.rawValue {
                self.audio?.stop()
                self.audio = nil
                self.emit("connecting", "Live listen paused while iPhone audio is in use.")
            } else if type == AVAudioSession.InterruptionType.ended.rawValue {
                guard AVAudioSession.InterruptionOptions(rawValue: options).contains(.shouldResume) else {
                    self.finish(type: "ended", message: "Live listen paused. Tap Live listen to resume.")
                    return
                }
                do {
                    try self.startAudio()
                    self.emit("connected", "Listening live")
                } catch {
                    self.finish(type: "error", message: audioFailure(error, activity: "restore live audio"))
                }
            }
        }
    }

    private func finish(type: String, message: String) {
        guard callID != nil else { return }
        watchdog?.cancel()
        watchdog = nil
        heartbeat?.cancel()
        heartbeat = nil
        let socket = self.socket
        self.socket = nil
        socket?.cancel(with: .normalClosure, reason: nil)
        session?.invalidateAndCancel()
        session = nil
        audio?.stop()
        audio = nil
        if ownsAudioSession {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            ownsAudioSession = false
        }
        connected = false
        emit(type, message)
        callID = nil
        callback = nil
    }

    private func emit(_ type: String, _ message: String) {
        guard let callback = callback, let id = callID,
              let data = try? JSONSerialization.data(withJSONObject: ["call_id": id, "type": type, "message": message]),
              let json = String(data: data, encoding: .utf8) else { return }
        json.withCString { callback($0) }
    }
}

@_cdecl("courtside_listen_start")
func courtsideListenStart(_ url: UnsafePointer<CChar>, _ token: UnsafePointer<CChar>, _ id: UnsafePointer<CChar>, _ callback: @escaping VoiceCallback) {
    let endpoint = String(cString: url)
    let bearer = String(cString: token)
    let callID = String(cString: id)
    DispatchQueue.main.async {
        guard !CallVoice.shared.isActive else {
            let json = "{\"call_id\":\"\(callID)\",\"type\":\"error\",\"message\":\"You are already joined to a call.\"}"
            json.withCString { callback($0) }
            return
        }
        CallListen.shared.start(url: endpoint, token: bearer, id: callID, callback: callback)
    }
}

@_cdecl("courtside_listen_stop")
func courtsideListenStop(_ id: UnsafePointer<CChar>) {
    CallListen.shared.stop(id: String(cString: id))
}
