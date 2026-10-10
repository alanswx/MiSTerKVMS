import AVFoundation
import CoreMedia
import Foundation
import KVMSCore

/// Records the session's video and audio to MP4 (H.264 + AAC) with
/// AVFoundation's hardware encoder.
///
/// Frames arrive on the core's receive thread. Pixels are copied into an
/// encoder buffer during the call (the pointer is only valid then); all
/// writer work happens on one serial queue. If the picture size changes
/// mid-recording, the current file is finished and a new part started.
final class Recorder: MediaSink, @unchecked Sendable {
    /// Called on the main queue when a file is finished (or failed).
    var onFinished: ((Result<URL, Error>) -> Void)?

    private let baseURL: URL
    private let queue = DispatchQueue(label: "recorder")
    private var part = 0

    // Writer state, only touched on `queue`.
    private var writer: AVAssetWriter?
    private var videoInput: AVAssetWriterInput?
    private var audioInput: AVAssetWriterInput?
    private var adaptor: AVAssetWriterInputPixelBufferAdaptor?
    private var size: (UInt32, UInt32) = (0, 0)
    private var startTimestamp: Int64?
    private var audioFormat: CMAudioFormatDescription?
    private var stopped = false

    /// `baseURL` without extension; parts get " (part 2)" etc.
    init(baseURL: URL) {
        self.baseURL = baseURL
    }

    static func defaultURL(hostName: String) -> URL {
        let movies = FileManager.default.urls(for: .moviesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("MiSTerKVMS", isDirectory: true)
        try? FileManager.default.createDirectory(at: movies, withIntermediateDirectories: true)
        let stamp = Date().formatted(.verbatim(
            "\(year: .defaultDigits)-\(month: .twoDigits)-\(day: .twoDigits) \(hour: .twoDigits(clock: .twentyFourHour, hourCycle: .zeroBased)).\(minute: .twoDigits).\(second: .twoDigits)",
            timeZone: .current, calendar: .current))
        let safe = hostName.replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: ":", with: "-")
        return movies.appendingPathComponent("\(safe) \(stamp)")
    }

    /// Finish the current file. Further frames are ignored.
    func stop() {
        queue.async { [self] in
            stopped = true
            finishWriter()
        }
    }

    // MARK: - MediaSink (receive thread)

    func videoFrame(data: UInt64, stride: UInt32, width: UInt32, height: UInt32, timestamp: Int64) {
        guard let src = UnsafeRawPointer(bitPattern: UInt(data)) else { return }
        queue.sync {
            guard !stopped else { return }
            if writer == nil || size != (width, height) {
                finishWriter()
                startWriter(width: Int(width), height: Int(height), timestamp: timestamp)
            }
            guard let writer, writer.status == .writing, let adaptor, let videoInput,
                  videoInput.isReadyForMoreMediaData, let pool = adaptor.pixelBufferPool,
                  let start = startTimestamp else { return }
            var buffer: CVPixelBuffer?
            guard CVPixelBufferPoolCreatePixelBuffer(nil, pool, &buffer) == kCVReturnSuccess,
                  let buffer else { return }
            CVPixelBufferLockBaseAddress(buffer, [])
            if let dst = CVPixelBufferGetBaseAddress(buffer) {
                let dstStride = CVPixelBufferGetBytesPerRow(buffer)
                let rowBytes = Int(width) * 4
                for row in 0..<Int(height) {
                    memcpy(dst + row * dstStride, src + row * Int(stride), rowBytes)
                }
            }
            CVPixelBufferUnlockBaseAddress(buffer, [])
            adaptor.append(buffer, withPresentationTime: Self.time(timestamp - start))
        }
    }

    func audioFrame(samples: [Float], channels: UInt32, sampleRate: UInt32, timestamp: Int64) {
        queue.async { [self] in
            guard !stopped, let writer, writer.status == .writing, let audioInput,
                  audioInput.isReadyForMoreMediaData, let start = startTimestamp,
                  timestamp >= start else { return }
            let stereo = Self.toStereo(samples, channels: Int(channels))
            if let sample = makeAudioSample(stereo, channels: 2, rate: sampleRate,
                                            at: Self.time(timestamp - start)) {
                audioInput.append(sample)
            }
        }
    }

    // MARK: - Writer (queue)

    private func startWriter(width: Int, height: Int, timestamp: Int64) {
        part += 1
        let url = part == 1
            ? baseURL.appendingPathExtension("mp4")
            : URL(fileURLWithPath: baseURL.path + " (part \(part))").appendingPathExtension("mp4")
        do {
            let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
            let video = AVAssetWriterInput(mediaType: .video, outputSettings: [
                AVVideoCodecKey: AVVideoCodecType.h264,
                AVVideoWidthKey: width,
                AVVideoHeightKey: height,
                AVVideoCompressionPropertiesKey: [
                    // Generous: desktop text and pixel art need it to stay crisp.
                    AVVideoAverageBitRateKey: max(8_000_000, width * height * 10),
                    AVVideoExpectedSourceFrameRateKey: 60,
                    AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
                ],
            ])
            video.expectsMediaDataInRealTime = true
            let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: video, sourcePixelBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey as String: width,
                kCVPixelBufferHeightKey as String: height,
            ])
            let audio = AVAssetWriterInput(mediaType: .audio, outputSettings: [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 48_000,
                AVNumberOfChannelsKey: 2,
                AVEncoderBitRateKey: 192_000,
            ])
            audio.expectsMediaDataInRealTime = true
            writer.add(video)
            writer.add(audio)
            guard writer.startWriting() else { throw writer.error ?? CocoaError(.fileWriteUnknown) }
            writer.startSession(atSourceTime: .zero)
            self.writer = writer
            videoInput = video
            audioInput = audio
            self.adaptor = adaptor
            size = (UInt32(width), UInt32(height))
            startTimestamp = timestamp
            audioFormat = nil
        } catch {
            report(.failure(error))
            stopped = true
        }
    }

    private func finishWriter() {
        guard let writer else { return }
        videoInput?.markAsFinished()
        audioInput?.markAsFinished()
        let url = writer.outputURL
        let group = DispatchGroup()
        group.enter()
        writer.finishWriting { group.leave() }
        group.wait()
        report(writer.status == .completed ? .success(url) : .failure(writer.error ?? CocoaError(.fileWriteUnknown)))
        self.writer = nil
        videoInput = nil
        audioInput = nil
        adaptor = nil
        startTimestamp = nil
    }

    private func report(_ result: Result<URL, Error>) {
        DispatchQueue.main.async { [onFinished] in onFinished?(result) }
    }

    private func makeAudioSample(_ samples: [Float], channels: UInt32, rate: UInt32, at time: CMTime) -> CMSampleBuffer? {
        let frames = samples.count / Int(max(channels, 1))
        guard frames > 0 else { return nil }
        if audioFormat == nil {
            var asbd = AudioStreamBasicDescription(
                mSampleRate: Float64(rate), mFormatID: kAudioFormatLinearPCM,
                mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
                mBytesPerPacket: 4 * channels, mFramesPerPacket: 1, mBytesPerFrame: 4 * channels,
                mChannelsPerFrame: channels, mBitsPerChannel: 32, mReserved: 0)
            CMAudioFormatDescriptionCreate(allocator: nil, asbd: &asbd, layoutSize: 0, layout: nil,
                                           magicCookieSize: 0, magicCookie: nil, extensions: nil,
                                           formatDescriptionOut: &audioFormat)
        }
        guard let audioFormat else { return nil }
        let bytes = samples.count * 4
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(allocator: nil, memoryBlock: nil, blockLength: bytes,
                                                 blockAllocator: nil, customBlockSource: nil, offsetToData: 0,
                                                 dataLength: bytes, flags: kCMBlockBufferAssureMemoryNowFlag,
                                                 blockBufferOut: &block) == noErr, let block else { return nil }
        samples.withUnsafeBytes { raw in
            _ = CMBlockBufferReplaceDataBytes(with: raw.baseAddress!, blockBuffer: block,
                                              offsetIntoDestination: 0, dataLength: bytes)
        }
        var sample: CMSampleBuffer?
        CMAudioSampleBufferCreateReadyWithPacketDescriptions(
            allocator: nil, dataBuffer: block, formatDescription: audioFormat, sampleCount: frames,
            presentationTimeStamp: time, packetDescriptions: nil, sampleBufferOut: &sample)
        return sample
    }

    /// The AAC track is stereo: duplicate mono, keep the first two of more.
    private static func toStereo(_ samples: [Float], channels: Int) -> [Float] {
        if channels == 2 { return samples }
        let frames = samples.count / max(channels, 1)
        var out = [Float](repeating: 0, count: frames * 2)
        for i in 0..<frames {
            out[i * 2] = samples[i * channels]
            out[i * 2 + 1] = samples[i * channels + min(1, channels - 1)]
        }
        return out
    }

    /// NDI timestamps are 100 ns units.
    private static func time(_ ticks: Int64) -> CMTime {
        CMTime(value: max(0, ticks), timescale: 10_000_000)
    }
}
