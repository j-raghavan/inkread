package dev.jraghavan.inkread

/**
 * Decides whether a `surfaceChanged` callback has to re-render the page (#186).
 *
 * Android delivers `surfaceChanged` more than once for a single surface — typically again once the
 * window has settled — with identical dimensions each time. The reader rendered on every one of
 * them, so opening a book drew page 0 twice: on the book #186 measured that is ~1.3s of core work
 * thrown away, before anything is on screen, plus a wasted EPD refresh.
 *
 * Skipping a repeat is not simply "same size, do nothing", because a surface that was destroyed and
 * recreated (leaving and returning to the reader) also comes back at the same size and *must*
 * redraw — a SurfaceView shows black until something is pushed to it. The two cases are told apart
 * by surface generation: [onSurfaceCreated] starts a new one, and a render is redundant only when
 * this generation has already been rendered at this exact size.
 *
 * Pure + host-testable — no Android types. All calls arrive on the engine thread except
 * [onSurfaceCreated], which the UI thread makes, so the generation counter is `@Volatile`.
 */
class SurfaceRenderGate {

    @Volatile
    private var generation = 0

    private var renderedGeneration = -1
    private var renderedWidth = 0
    private var renderedHeight = 0

    /** A new surface exists: whatever was drawn before is gone, so the next size must render. */
    fun onSurfaceCreated() {
        generation++
    }

    /**
     * Report whether a `surfaceChanged` at `width` x `height` needs to render, recording it when it
     * does. `documentOpen` is false before a document exists — nothing can be redundant then, since
     * the work that call does is the open itself.
     */
    fun needsRender(width: Int, height: Int, documentOpen: Boolean): Boolean {
        val redundant = documentOpen &&
            generation == renderedGeneration &&
            width == renderedWidth &&
            height == renderedHeight
        if (redundant) {
            return false
        }
        renderedGeneration = generation
        renderedWidth = width
        renderedHeight = height
        return true
    }
}
