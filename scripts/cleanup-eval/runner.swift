// Runs cleanup jobs through Apple's on-device model exactly like Handy's
// bridge (src-tauri/swift/apple_intelligence.swift): instructions = system
// prompt, structured CleanedTranscript generation with plain-text fallback.
//   runner jobs.json out.json [--plain]
import Foundation
import FoundationModels

@Generable
struct CleanedTranscript: Sendable {
    let cleanedText: String
}

struct Job: Codable { let id: String; let system: String; let user: String }
struct Out: Codable { let id: String; let output: String; let ms: Int; let error: String? }

@main
struct Runner {
    static func main() async throws {
        let args = CommandLine.arguments
        let plain = args.contains("--plain")
        let options = args.contains("--greedy") ? GenerationOptions(sampling: .greedy) : GenerationOptions()
        let jobs = try JSONDecoder().decode([Job].self, from: Data(contentsOf: URL(fileURLWithPath: args[1])))
        var outs: [Out] = []
        for job in jobs {
            let t0 = Date()
            var text = ""
            var err: String? = nil
            do {
                let session = LanguageModelSession(model: .default, instructions: job.system)
                if plain {
                    text = try await session.respond(to: job.user, options: options).content
                } else {
                    do {
                        text = try await session.respond(to: job.user, generating: CleanedTranscript.self, options: options).content.cleanedText
                    } catch {
                        text = try await session.respond(to: job.user, options: options).content
                    }
                }
            } catch { err = "\(error)" }
            let ms = Int(Date().timeIntervalSince(t0) * 1000)
            outs.append(Out(id: job.id, output: text, ms: ms, error: err))
            FileHandle.standardError.write("\(outs.count)/\(jobs.count) \(ms)ms\r".data(using: .utf8)!)
        }
        try JSONEncoder().encode(outs).write(to: URL(fileURLWithPath: args[2]))
    }
}
