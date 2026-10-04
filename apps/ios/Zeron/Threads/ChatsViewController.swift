import UIKit

/// The Chat tab: general conversations, the desktop Chat panel's. Each is a
/// project-less chat in the desktop's general-chat folder, where the agent
/// searches the web and touches no files. Newest first; the tab's "New chat"
/// accessory starts one in the same folder.
final class ChatsViewController: SessionListController {
    private let empty = UIStackView()
    private let emptyTitle = UILabel()
    private let emptyBody = UILabel()

    override func viewDidLoad() {
        organizable = false
        super.viewDidLoad()
        title = "Chat"
        navigationItem.largeTitleDisplayMode = .always

        let mark = UIImageView(image: UIImage(systemName: "bubble.left.and.text.bubble.right", withConfiguration: UIImage.SymbolConfiguration(pointSize: 30, weight: .regular)))
        mark.tintColor = Palette.tertiary
        emptyTitle.font = Fonts.ui(.sansSemibold, 17)
        emptyTitle.textColor = Palette.text
        emptyBody.font = Fonts.ui(.sansMedium, 14)
        emptyBody.textColor = Palette.secondary
        emptyBody.numberOfLines = 0
        emptyBody.textAlignment = .center
        for v in [mark, emptyTitle, emptyBody] { empty.addArrangedSubview(v) }
        empty.axis = .vertical
        empty.alignment = .center
        empty.spacing = 10
        empty.setCustomSpacing(16, after: mark)
        empty.translatesAutoresizingMaskIntoConstraints = false
        let backdrop = UIView()
        backdrop.addSubview(empty)
        NSLayoutConstraint.activate([
            empty.centerXAnchor.constraint(equalTo: backdrop.centerXAnchor),
            empty.centerYAnchor.constraint(equalTo: backdrop.centerYAnchor, constant: -40),
            empty.widthAnchor.constraint(lessThanOrEqualTo: backdrop.widthAnchor, constant: -64),
            empty.widthAnchor.constraint(lessThanOrEqualToConstant: 340),
        ])
        collectionView.backgroundView = backdrop
        updateEmptyState(animated: false)
    }

    override func buildSections() -> [(id: String, header: String?, folders: [FolderRowVM], sessions: [SessionRowVM])] {
        [("chats", nil, [], app.generalChats)]
    }

    override func reload(animated: Bool) {
        super.reload(animated: animated)
        updateEmptyState(animated: animated)
    }

    /// Honest about why the list is empty: until the desktop has made a
    /// general chat, the phone doesn't know where to start one.
    private func updateEmptyState(animated: Bool) {
        if let home = app.generalHome {
            emptyTitle.text = "No chats"
            emptyBody.text = "Ask anything. Chats run on \(app.deviceName(home.deviceId)) with web search, and never touch your files."
        } else {
            emptyTitle.text = "Start on your desktop"
            emptyBody.text = "Send one message from the Chat panel in Zeron on your desktop. After that, your chats show up here and you can start new ones from your phone."
        }
        let alpha: CGFloat = app.generalChats.isEmpty ? 1 : 0
        guard empty.alpha != alpha else { return }
        UIView.animate(withDuration: animated && view.window != nil ? 0.25 : 0) { self.empty.alpha = alpha }
    }
}
