package com.ospab.overnet

import android.graphics.Canvas
import android.graphics.ColorFilter
import android.graphics.Paint
import android.graphics.Path
import android.graphics.PixelFormat
import android.graphics.RectF
import android.graphics.drawable.Drawable
import kotlin.math.cos
import kotlin.math.sin

/**
 * The interface icons, drawn as thin strokes on a 24-unit grid (the look of
 * Phosphor's regular weight, as the design system asks) — no icon font or
 * image files in the APK.
 */
class Glyph(private val kind: Kind, color: Int) : Drawable() {
    enum class Kind { BACK, FORWARD, RELOAD, CLOSE, PLUS, SHARE, MENU, HOME, CIRCUITS, COPY, LINK, IMAGE, NETWORK, TABS }

    private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
        this.color = color
    }
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.FILL
        this.color = color
    }

    var color: Int
        get() = stroke.color
        set(v) {
            stroke.color = v
            fill.color = v
            invalidateSelf()
        }

    override fun draw(canvas: Canvas) {
        val b = bounds
        val s = minOf(b.width(), b.height()) / 24f
        canvas.save()
        canvas.translate(b.left + (b.width() - 24 * s) / 2, b.top + (b.height() - 24 * s) / 2)
        canvas.scale(s, s)
        stroke.strokeWidth = 1.7f
        val p = Path()
        when (kind) {
            Kind.BACK -> {
                p.moveTo(20f, 12f); p.lineTo(4f, 12f)
                p.moveTo(10f, 6f); p.lineTo(4f, 12f); p.lineTo(10f, 18f)
            }
            Kind.FORWARD -> {
                p.moveTo(4f, 12f); p.lineTo(20f, 12f)
                p.moveTo(14f, 6f); p.lineTo(20f, 12f); p.lineTo(14f, 18f)
            }
            Kind.RELOAD -> {
                p.addArc(RectF(5f, 5f, 19f, 19f), -50f, 300f)
                arrowHead(p, 12f + 7f * cos(rad(250f)), 12f + 7f * sin(rad(250f)), rad(250f + 90f))
            }
            Kind.CLOSE -> {
                p.moveTo(6f, 6f); p.lineTo(18f, 18f)
                p.moveTo(18f, 6f); p.lineTo(6f, 18f)
            }
            Kind.PLUS -> {
                p.moveTo(12f, 4.5f); p.lineTo(12f, 19.5f)
                p.moveTo(4.5f, 12f); p.lineTo(19.5f, 12f)
            }
            Kind.SHARE -> {
                p.moveTo(8.5f, 9f); p.lineTo(6f, 9f); p.lineTo(6f, 20f); p.lineTo(18f, 20f); p.lineTo(18f, 9f); p.lineTo(15.5f, 9f)
                p.moveTo(12f, 14f); p.lineTo(12f, 3.5f)
                p.moveTo(8.5f, 7f); p.lineTo(12f, 3.5f); p.lineTo(15.5f, 7f)
            }
            Kind.MENU -> {
                canvas.drawCircle(12f, 5.5f, 1.6f, fill)
                canvas.drawCircle(12f, 12f, 1.6f, fill)
                canvas.drawCircle(12f, 18.5f, 1.6f, fill)
            }
            Kind.HOME -> {
                p.moveTo(4f, 11f); p.lineTo(12f, 4f); p.lineTo(20f, 11f)
                p.moveTo(6.5f, 9f); p.lineTo(6.5f, 20f); p.lineTo(17.5f, 20f); p.lineTo(17.5f, 9f)
                p.moveTo(10f, 20f); p.lineTo(10f, 14.5f); p.lineTo(14f, 14.5f); p.lineTo(14f, 20f)
            }
            Kind.CIRCUITS -> {
                // Two arrows chasing each other: the route changes.
                p.addArc(RectF(4.5f, 4.5f, 19.5f, 19.5f), 200f, 130f)
                arrowHead(p, 12f + 7.5f * cos(rad(330f)), 12f + 7.5f * sin(rad(330f)), rad(330f + 90f))
                p.addArc(RectF(4.5f, 4.5f, 19.5f, 19.5f), 20f, 130f)
                arrowHead(p, 12f + 7.5f * cos(rad(150f)), 12f + 7.5f * sin(rad(150f)), rad(150f + 90f))
            }
            Kind.COPY -> {
                p.addRoundRect(RectF(8.5f, 8.5f, 19.5f, 19.5f), 1.5f, 1.5f, Path.Direction.CW)
                p.moveTo(15.5f, 8.5f); p.lineTo(15.5f, 4.5f); p.lineTo(4.5f, 4.5f); p.lineTo(4.5f, 15.5f); p.lineTo(8.5f, 15.5f)
            }
            Kind.LINK -> {
                p.addRoundRect(RectF(3f, 9f, 13.5f, 15f), 3f, 3f, Path.Direction.CW)
                p.addRoundRect(RectF(10.5f, 9f, 21f, 15f), 3f, 3f, Path.Direction.CW)
            }
            Kind.IMAGE -> {
                p.addRoundRect(RectF(3.5f, 5f, 20.5f, 19f), 1.5f, 1.5f, Path.Direction.CW)
                p.moveTo(3.5f, 16f); p.lineTo(9f, 11f); p.lineTo(13.5f, 15.5f); p.lineTo(16f, 13f); p.lineTo(20.5f, 17f)
                canvas.drawCircle(15.5f, 9f, 1.3f, fill)
            }
            Kind.NETWORK -> {
                // The overnet mark: a ring in a ring.
                p.addCircle(12f, 12f, 8f, Path.Direction.CW)
                p.addCircle(12f, 12f, 3.4f, Path.Direction.CW)
            }
            Kind.TABS -> {
                p.addRoundRect(RectF(4f, 4f, 20f, 20f), 3f, 3f, Path.Direction.CW)
            }
        }
        canvas.drawPath(p, stroke)
        canvas.restore()
    }

    // Two short strokes at the tip of an arc, around the direction it travels.
    private fun arrowHead(p: Path, x: Float, y: Float, dir: Float) {
        val len = 4f
        for (a in floatArrayOf(dir + 2.5f, dir - 2.5f)) {
            p.moveTo(x, y)
            p.lineTo(x + len * cos(a), y + len * sin(a))
        }
    }

    private fun rad(deg: Float) = Math.toRadians(deg.toDouble()).toFloat()

    override fun setAlpha(alpha: Int) {
        stroke.alpha = alpha
        fill.alpha = alpha
    }

    override fun setColorFilter(colorFilter: ColorFilter?) {
        stroke.colorFilter = colorFilter
        fill.colorFilter = colorFilter
    }

    @Deprecated("Deprecated in Drawable")
    override fun getOpacity() = PixelFormat.TRANSLUCENT
}
