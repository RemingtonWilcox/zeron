import AVFoundation
import Speech

/// On-device dictation for the composer: microphone → SpeechAnalyzer. Reports
/// the transcript as committed (final) text plus the volatile tail that may
/// still change. Nothing leaves the phone.
@MainActor
final class Dictation {
    enum Failure: LocalizedError {
        case micDenied, speechDenied, noMicrophone

        var errorDescription: String? {
            switch self {
            case .micDenied: "Microphone access is off in Settings"
            case .speechDenied: "Speech recognition is off in Settings"
            case .noMicrophone: "No microphone available"
            }
        }
    }

    /// (committed, volatile) transcript so far.
    var onText: ((String, String) -> Void)?
    /// The analyzer stopped on its own (error, or audio went away).
    var onFailure: ((Error) -> Void)?

    private var committed = ""
    private var volatile = ""
    private let engine = AVAudioEngine()
    private var analyzer: SpeechAnalyzer?
    private var input: AsyncStream<AnalyzerInput>.Continuation?
    private var results: Task<Void, Never>?
    private var stopped = false

    /// Ask for access, prepare the model (iOS downloads it once if needed)
    /// and start listening. Throws a `Failure` with a short, honest message.
    func start() async throws {
        guard await AVAudioApplication.requestRecordPermission() else { throw Failure.micDenied }
        let speech = await withCheckedContinuation { done in
            SFSpeechRecognizer.requestAuthorization { done.resume(returning: $0) }
        }
        guard speech == .authorized else { throw Failure.speechDenied }

        // SpeechTranscriber where the hardware supports it; the dictation
        // model otherwise. Device language, falling back to US English.
        let module: any SpeechModule
        if SpeechTranscriber.isAvailable {
            let locale = await SpeechTranscriber.supportedLocale(equivalentTo: .current) ?? Locale(identifier: "en-US")
            let t = SpeechTranscriber(locale: locale, preset: .progressiveTranscription)
            module = t
            results = consume(t.results) { (String($0.text.characters), $0.isFinal) }
        } else {
            let locale = await DictationTranscriber.supportedLocale(equivalentTo: .current) ?? Locale(identifier: "en-US")
            let d = DictationTranscriber(locale: locale, preset: .progressiveShortDictation)
            module = d
            results = consume(d.results) { (String($0.text.characters), $0.isFinal) }
        }
        if let install = try await AssetInventory.assetInstallationRequest(supporting: [module]) {
            try await install.downloadAndInstall()
        }
        guard !stopped else { return }

        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record, mode: .measurement, options: .duckOthers)
        try session.setActive(true, options: .notifyOthersOnDeactivation)
        let mic = engine.inputNode.outputFormat(forBus: 0)
        guard mic.sampleRate > 0, mic.channelCount > 0 else {
            teardownAudio()
            throw Failure.noMicrophone
        }
        let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [module], considering: mic) ?? mic
        let (stream, continuation) = AsyncStream.makeStream(of: AnalyzerInput.self)
        input = continuation
        let analyzer = SpeechAnalyzer(modules: [module])
        self.analyzer = analyzer
        try await analyzer.start(inputSequence: stream)
        guard !stopped else { return teardownAudio() }
        engine.inputNode.installTap(onBus: 0, bufferSize: 4096, format: mic, block: Self.tap(from: mic, to: format, into: continuation))
        engine.prepare()
        try engine.start()
    }

    /// Stop listening and wait for the last words to be finalized.
    func finish() async {
        guard !stopped else { return }
        stopped = true
        teardownAudio()
        // Still preparing: no analyzer, so the results would never end.
        guard let analyzer else { return results?.cancel() ?? () }
        try? await analyzer.finalizeAndFinishThroughEndOfInput()
        await results?.value
        if !volatile.isEmpty {
            committed += volatile
            volatile = ""
            onText?(committed, "")
        }
    }

    /// Stop now and drop whatever is pending.
    func cancel() async {
        stopped = true
        teardownAudio()
        results?.cancel()
        await analyzer?.cancelAndFinishNow()
    }

    private func consume<S: AsyncSequence & Sendable>(_ seq: S, _ read: @escaping (S.Element) -> (String, Bool)) -> Task<Void, Never> {
        Task { [weak self] in
            do {
                for try await r in seq {
                    guard let self else { return }
                    let (text, final) = read(r)
                    if final {
                        self.committed += text
                        self.volatile = ""
                    } else {
                        self.volatile = text
                    }
                    self.onText?(self.committed, self.volatile)
                }
            } catch {
                guard let self, !self.stopped, !Task.isCancelled else { return }
                self.onFailure?(error)
            }
        }
    }

    private func teardownAudio() {
        if engine.isRunning { engine.stop() }
        engine.inputNode.removeTap(onBus: 0)
        input?.finish()
        input = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    /// Built outside the main actor: the tap runs on the audio thread.
    nonisolated private static func tap(from mic: AVAudioFormat, to format: AVAudioFormat, into out: AsyncStream<AnalyzerInput>.Continuation) -> AVAudioNodeTapBlock {
        let converter = mic == format ? nil : AVAudioConverter(from: mic, to: format)
        converter?.primeMethod = .none
        return { buffer, _ in
            guard let converter else {
                out.yield(AnalyzerInput(buffer: buffer))
                return
            }
            let capacity = AVAudioFrameCount((Double(buffer.frameLength) * format.sampleRate / mic.sampleRate).rounded(.up))
            guard let converted = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: max(1, capacity)) else { return }
            var fed = false
            var error: NSError?
            converter.convert(to: converted, error: &error) { _, status in
                if fed {
                    status.pointee = .noDataNow
                    return nil
                }
                fed = true
                status.pointee = .haveData
                return buffer
            }
            if error == nil, converted.frameLength > 0 { out.yield(AnalyzerInput(buffer: converted)) }
        }
    }
}
