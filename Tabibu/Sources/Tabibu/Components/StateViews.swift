// Empty + error states — design.md §6: every data view defines all states.

import SwiftUI

struct EmptyState: View {
    var lucide: String? = nil
    var systemImage: String? = nil
    let title: String
    let message: String
    var actionTitle: String? = nil
    var action: (() -> Void)? = nil

    var body: some View {
        VStack(spacing: Space.m) {
            Group {
                if let lucide {
                    LucideIcon(name: lucide, size: 44)
                } else if let systemImage {
                    Image(systemName: systemImage)
                        .font(.system(size: 40, weight: .light))
                }
            }
            .foregroundStyle(.secondary)
            Text(title)
                .font(.title3.weight(.semibold))
            Text(message)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 440)
            if let actionTitle, let action {
                Button(actionTitle, action: action)
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .padding(.top, Space.s)
            }
        }
        .padding(Space.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct ErrorState: View {
    let message: String
    var retryTitle: String = "Try Again"
    var retry: (() -> Void)? = nil

    var body: some View {
        VStack(spacing: Space.m) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 36, weight: .light))
                .foregroundStyle(.orange)
            Text("Something went wrong")
                .font(.title3.weight(.semibold))
            Text(message)
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 480)
                .textSelection(.enabled)
            if let retry {
                Button(retryTitle, action: retry)
            }
        }
        .padding(Space.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
