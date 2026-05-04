#!/usr/bin/env swift

import Foundation
import Dispatch

#if canImport(FoundationModels)
import FoundationModels

@available(macOS 26.0, *)
@Generable
struct RoutingDecision: Codable {
    var action: String
    var confidence: Double
    var reason: String
}

@available(macOS 26.0, *)
func runRouter() async {
    let input = String(
        data: FileHandle.standardInput.readDataToEndOfFile(),
        encoding: .utf8
    )?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

    guard !input.isEmpty else {
        emitAndExit(
            action: "reply_normally",
            confidence: 0,
            reason: "empty input"
        )
    }

    let instructions = """
    You classify messages for a dispatch-only coding coordinator.

    Return action "dispatch_worker" when the user is asking for code, UI, layout, styling, bug-fix, or implementation changes.
    Return action "reply_normally" when the user is asking for explanation, summary, review, discussion, or general conversation.

    Treat phrasing like "can we make", "too square", "layout feels off", "broken", "jumpy", "compact on mobile", "padding", "overflow", "scroll", and "the page is wrong" as implementation requests.

    Be concise. Confidence must be between 0 and 1.
    """

    do {
        let session = LanguageModelSession(instructions: instructions)
        let response = try await session.respond(
            to: input,
            generating: RoutingDecision.self
        )
        let decision = response.content
        let action = decision.action == "dispatch_worker" ? "dispatch_worker" : "reply_normally"
        emitAndExit(
            action: action,
            confidence: decision.confidence,
            reason: decision.reason
        )
    } catch {
        fputs("apple_dispatch_router error: \(error)\n", stderr)
        Foundation.exit(2)
    }
}

@available(macOS 26.0, *)
func emitAndExit(action: String, confidence: Double, reason: String) -> Never {
    let output = RoutingDecision(
        action: action,
        confidence: confidence,
        reason: reason
    )
    let encoder = JSONEncoder()
    guard let data = try? encoder.encode(output),
        let json = String(data: data, encoding: .utf8)
    else {
        fputs("apple_dispatch_router failed to encode output\n", stderr)
        Foundation.exit(3)
    }
    print(json)
    Foundation.exit(0)
}

if #available(macOS 26.0, *) {
    Task {
        await runRouter()
    }
    dispatchMain()
} else {
    fputs("FoundationModels requires macOS 26 or later\n", stderr)
    Foundation.exit(2)
}

#else
fputs("FoundationModels framework unavailable\n", stderr)
Foundation.exit(2)
#endif
