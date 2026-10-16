import QuartzCore
import SwiftUI
import UIKit

/// The video, drawn by the core into this view's Metal layer, which is also
/// a relative touchpad (IPHONE.md §5.3):
///
/// - one finger drag: move the mouse; tap: left click; two-finger tap: right
///   click; double tap: double click; tap then drag: click, then hold the
///   left button while dragging
/// - pinch: zoom the picture; two-finger drag: pan it
/// - three-finger swipe left/right: next/previous MiSTer; three-finger tap:
///   the controls menu
final class TouchpadView: UIView, UIGestureRecognizerDelegate {
    weak var model: AppModel?
    var onMenu: (() -> Void)?
    var onSwitch: ((Int) -> Void)?

    override class var layerClass: AnyClass { CAMetalLayer.self }

    private var lastMove: CGPoint = .zero
    private var lastDrag: CGPoint = .zero
    private var lastPan: CGPoint = .zero

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        isMultipleTouchEnabled = true

        let move = UIPanGestureRecognizer(target: self, action: #selector(movePointer(_:)))
        move.maximumNumberOfTouches = 1
        let drag = UILongPressGestureRecognizer(target: self, action: #selector(dragPointer(_:)))
        drag.numberOfTapsRequired = 1
        drag.minimumPressDuration = 0
        let click = UITapGestureRecognizer(target: self, action: #selector(click(_:)))
        let rightClick = UITapGestureRecognizer(target: self, action: #selector(rightClick(_:)))
        rightClick.numberOfTouchesRequired = 2
        let pinch = UIPinchGestureRecognizer(target: self, action: #selector(zoomPicture(_:)))
        let pan = UIPanGestureRecognizer(target: self, action: #selector(panPicture(_:)))
        pan.minimumNumberOfTouches = 2
        pan.maximumNumberOfTouches = 2
        let menu = UITapGestureRecognizer(target: self, action: #selector(showMenu(_:)))
        menu.numberOfTouchesRequired = 3
        let next = UISwipeGestureRecognizer(target: self, action: #selector(swipe(_:)))
        next.numberOfTouchesRequired = 3
        next.direction = .left
        let previous = UISwipeGestureRecognizer(target: self, action: #selector(swipe(_:)))
        previous.numberOfTouchesRequired = 3
        previous.direction = .right

        // A tap waits to see whether a drag follows (tap-and-drag holds the
        // button, like a laptop trackpad); a moving finger is a plain move.
        click.require(toFail: drag)
        move.require(toFail: drag)
        menu.require(toFail: next)
        menu.require(toFail: previous)
        for g in [move, drag, click, rightClick, pinch, pan, menu, next, previous] as [UIGestureRecognizer] {
            g.delegate = self
            addGestureRecognizer(g)
        }
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        guard let model else { return }
        if window != nil {
            (layer as? CAMetalLayer)?.contentsScale = window?.screen.scale ?? 2
            model.attach(view: self)
        } else {
            model.detach(view: self)
        }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        if window != nil { model?.resized(view: self) }
    }

    // MARK: - Gestures

    /// Faster finger movement moves the pointer further, so both small
    /// adjustments and crossing the screen work.
    private func accelerated(_ d: CGPoint) -> (Double, Double) {
        let speed = hypot(d.x, d.y)
        let gain = min(1 + speed / 6, 4)
        return (Double(d.x * gain), Double(d.y * gain))
    }

    @objc private func movePointer(_ g: UIPanGestureRecognizer) {
        let t = g.translation(in: self)
        if g.state == .began { lastMove = .zero }
        let d = CGPoint(x: t.x - lastMove.x, y: t.y - lastMove.y)
        lastMove = t
        let (dx, dy) = accelerated(d)
        model?.move(dx: dx, dy: dy)
    }

    @objc private func dragPointer(_ g: UILongPressGestureRecognizer) {
        let p = g.location(in: self)
        switch g.state {
        case .began:
            lastDrag = p
            // The tap before this press was held back to see whether a drag
            // follows: click it now, so a double tap is a double click and
            // tap-and-drag is click, then drag.
            model?.click(.left)
            model?.button(.left, down: true)
        case .changed:
            let (dx, dy) = accelerated(CGPoint(x: p.x - lastDrag.x, y: p.y - lastDrag.y))
            lastDrag = p
            model?.move(dx: dx, dy: dy)
        case .ended, .cancelled, .failed:
            model?.button(.left, down: false)
        default:
            break
        }
    }

    @objc private func click(_ g: UITapGestureRecognizer) { model?.click(.left) }
    @objc private func rightClick(_ g: UITapGestureRecognizer) { model?.click(.right) }
    @objc private func showMenu(_ g: UITapGestureRecognizer) { onMenu?() }

    @objc private func swipe(_ g: UISwipeGestureRecognizer) {
        onSwitch?(g.direction == .left ? 1 : -1)
    }

    @objc private func zoomPicture(_ g: UIPinchGestureRecognizer) {
        guard g.state == .changed else { return }
        model?.zoom(by: Double(g.scale))
        g.scale = 1
    }

    @objc private func panPicture(_ g: UIPanGestureRecognizer) {
        let t = g.translation(in: self)
        if g.state == .began { lastPan = .zero }
        let d = CGPoint(x: t.x - lastPan.x, y: t.y - lastPan.y)
        lastPan = t
        guard bounds.width > 0, bounds.height > 0 else { return }
        // Content follows the fingers: dragging right shows what's to the left.
        model?.pan(dx: Double(-d.x / bounds.width), dy: Double(-d.y / bounds.height))
    }

    func gestureRecognizer(_ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool {
        // Pinch and two-finger pan together (zoom while moving around).
        (g is UIPinchGestureRecognizer && other is UIPanGestureRecognizer)
            || (g is UIPanGestureRecognizer && other is UIPinchGestureRecognizer)
    }
}

struct TouchpadVideo: UIViewRepresentable {
    let model: AppModel
    var onMenu: () -> Void
    var onSwitch: (Int) -> Void

    func makeUIView(context: Context) -> TouchpadView {
        let view = TouchpadView(frame: .zero)
        view.model = model
        view.onMenu = onMenu
        view.onSwitch = onSwitch
        return view
    }

    func updateUIView(_ view: TouchpadView, context: Context) {
        view.onMenu = onMenu
        view.onSwitch = onSwitch
    }

    static func dismantleUIView(_ view: TouchpadView, coordinator: ()) {
        MainActor.assumeIsolated { view.model?.detach(view: view) }
    }
}
