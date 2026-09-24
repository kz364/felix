import Dispatch
import Foundation
import FoundationModels

// MARK: - Swift implementation for Apple LLM integration
// This file is compiled via Cargo build script for Apple Silicon targets

private typealias ResponsePointer = UnsafeMutablePointer<AppleLLMResponse>

private func duplicateCString(_ text: String) -> UnsafeMutablePointer<CChar>? {
    return text.withCString { basePointer in
        guard let duplicated = strdup(basePointer) else {
            return nil
        }
        return duplicated
    }
}

private func truncatedText(_ text: String, limit: Int) -> String {
    guard limit > 0 else { return text }
    let words = text.split(
        maxSplits: .max,
        omittingEmptySubsequences: true,
        whereSeparator: { $0.isWhitespace || $0.isNewline }
    )
    if words.count <= limit {
        return text
    }
    return words.prefix(limit).joined(separator: " ")
}

// MARK: - Prewarmed session

/// One session prewarmed while the user is still speaking, handed to the next
/// request with the same instructions. Sessions keep their transcript, so each
/// is used once.
private final class WarmSessionStore: @unchecked Sendable {
    private let lock = NSLock()
    private var instructions: String?
    private var session: AnyObject?

    func put(_ session: AnyObject, instructions: String) {
        lock.lock()
        defer { lock.unlock() }
        self.session = session
        self.instructions = instructions
    }

    func take(instructions: String) -> AnyObject? {
        lock.lock()
        defer { lock.unlock() }
        guard self.instructions == instructions, let session = session else {
            return nil
        }
        self.session = nil
        self.instructions = nil
        return session
    }
}

private let warmSessions = WarmSessionStore()

/// Create and prewarm a session for the next cleanup. Loading the model and
/// the instructions ahead of time saves ~100 ms of the ~500 ms before the
/// first output token; also caching the fixed start of the user message
/// saves ~20 ms more (fm/prefill.swift in the eval scratchpad).
@_cdecl("prewarm_apple_session")
public func prewarmAppleSession(
    _ systemPrompt: UnsafePointer<CChar>,
    _ promptPrefix: UnsafePointer<CChar>
) {
    let instructions = String(cString: systemPrompt)
    let prefix = String(cString: promptPrefix)
    guard #available(macOS 26.0, *) else { return }
    let model = SystemLanguageModel.default
    guard model.availability == .available else { return }

    let session = LanguageModelSession(model: model, instructions: instructions)
    session.prewarm()
    if !prefix.isEmpty {
        Task.detached(priority: .utility) {
            try? await Task.sleep(nanoseconds: 300_000_000)
            session.prewarm(promptPrefix: Prompt(prefix))
        }
    }
    warmSessions.put(session, instructions: instructions)
}

@_cdecl("is_apple_intelligence_available")
public func isAppleIntelligenceAvailable() -> Int32 {
    guard #available(macOS 26.0, *) else {
        return 0
    }

    let model = SystemLanguageModel.default
    switch model.availability {
    case .available:
        return 1
    case .unavailable:
        return 0
    }
}

@_cdecl("process_text_with_system_prompt_apple")
public func processTextWithSystemPrompt(
    _ systemPrompt: UnsafePointer<CChar>,
    _ userContent: UnsafePointer<CChar>,
    maxTokens: Int32
) -> UnsafeMutablePointer<AppleLLMResponse> {
    let swiftSystemPrompt = String(cString: systemPrompt)
    let swiftUserContent = String(cString: userContent)
    let responsePtr = ResponsePointer.allocate(capacity: 1)
    responsePtr.initialize(to: AppleLLMResponse(response: nil, success: 0, error_message: nil))

    guard #available(macOS 26.0, *) else {
        responsePtr.pointee.error_message = duplicateCString(
            "Apple Intelligence requires macOS 26 or newer."
        )
        return responsePtr
    }

    let model = SystemLanguageModel.default
    guard model.availability == .available else {
        responsePtr.pointee.error_message = duplicateCString(
            "Apple Intelligence is not currently available on this device."
        )
        return responsePtr
    }

    let tokenLimit = max(0, Int(maxTokens))
    let semaphore = DispatchSemaphore(value: 0)

    // Thread-safe container to pass results from async task back to calling thread
    final class ResultBox: @unchecked Sendable {
        var response: String?
        var error: String?
    }
    let box = ResultBox()

    Task.detached(priority: .userInitiated) {
        defer { semaphore.signal() }
        do {
            let session =
                (warmSessions.take(instructions: swiftSystemPrompt) as? LanguageModelSession)
                ?? LanguageModelSession(model: model, instructions: swiftSystemPrompt)
            // Plain text generation: ~10% faster than guided generation of a
            // wrapper struct, with the same cleanup quality in the prompt
            // eval (scripts/cleanup-eval).
            var output = try await session.respond(to: swiftUserContent).content

            if tokenLimit > 0 {
                output = truncatedText(output, limit: tokenLimit)
            }
            box.response = output
        } catch {
            box.error = error.localizedDescription
        }
    }

    semaphore.wait()

    // Write to responsePtr on the calling thread after task completes
    if let response = box.response {
        responsePtr.pointee.response = duplicateCString(response)
        responsePtr.pointee.success = 1
    } else {
        responsePtr.pointee.error_message = duplicateCString(box.error ?? "Unknown error")
    }

    return responsePtr
}

@_cdecl("free_apple_llm_response")
public func freeAppleLLMResponse(_ response: UnsafeMutablePointer<AppleLLMResponse>?) {
    guard let response = response else { return }

    if let responseStr = response.pointee.response {
        free(UnsafeMutablePointer(mutating: responseStr))
    }

    if let errorStr = response.pointee.error_message {
        free(UnsafeMutablePointer(mutating: errorStr))
    }

    response.deallocate()
}