"""OpenShell lock artwork, shared by the native screen and image previews.

This renderer receives public display state only, never password contents.
Everything clickable has its rectangle in targets(), which both the painter
and the native screen's mouse handling use, so they cannot drift apart.
"""
from dataclasses import dataclass, field
from datetime import datetime
import math

TEXT = {
    'locked': 'Session locked',
    'password': 'Password',
    'hint': 'Press Enter to unlock',
    'checking': 'Checking…',
    'failed': 'Incorrect password. Try again.',
    'caps': 'Caps Lock is on',
    'suspend': 'Sleep',
    'reboot': 'Restart',
    'shutdown': 'Shut down',
    'confirm-reboot': 'Click again to restart',
    'confirm-shutdown': 'Click again to shut down',
    'charging': 'Charging',
}
POWER_ACTIONS = ('suspend', 'reboot', 'shutdown')


def strings(language):
    return TEXT


DATES = {
    'en': ('Monday Tuesday Wednesday Thursday Friday Saturday Sunday', 'January February March April May June July August September October November December'),
}


def date_label(now, language):
    days, months = DATES['en']
    return f'{days.split()[now.weekday()]}, {now.day:02d} {months.split()[now.month-1]}'


def clock_label(now, clock24h):
    if clock24h:
        return now.strftime('%H:%M'), ''
    return f'{now.hour % 12 or 12}:{now.minute:02d}', 'AM' if now.hour < 12 else 'PM'


@dataclass
class ScreenState:
    name: str = 'OpenShell'
    language: str = 'en'
    layout: str = ''
    count: int = 0
    busy: bool = False
    failed: bool = False
    caps: bool = False
    prompt: str = ''
    now: datetime = field(default_factory=datetime.now)
    clock24h: bool = True
    show_name: bool = True
    # (percent, charging) or None when hidden or there is no battery.
    battery: tuple = None
    # Power actions offered on this screen, a subset of POWER_ACTIONS.
    actions: tuple = ()
    hover: str = None
    # The control under a held mouse button, drawn pressed.
    pressed: str = None
    # A restart/shut down waiting for its confirming second click.
    armed: str = None
    notice: str = ''
    # The account picture (~/.face) as a cairo surface, or None for initials.
    photo: object = None
    # The look chosen in Settings ▸ Lock screen ▸ Look.
    style: object = field(default_factory=lambda: LockStyle())


# Near-black, neutral palette: the screen should recede, not glow.
INK = '#e6e8eb'
SOFT = '#9aa0a8'
DIM = '#5f656e'
FAINT = '#2a2d33'
FIELD = '#111317'
RAISED = '#1b1d22'
DANGER = '#ef8a8a'
WARNING = '#e2c07a'
CLOCK_WEIGHT = {'light': 300, 'regular': 400, 'bold': 600}


@dataclass
class LockStyle:
    """Background, colors and clock (lock_settings.py keys). The defaults
    draw the usual screen, and so does a picture that could not be read."""
    background: str = 'default'  # default, color, gradient or picture
    color: str = '#16202e'
    color2: str = '#3b2a55'
    # The picture as a cairo surface, or None.
    picture: object = None
    dim: int = 35
    text: str = INK
    accent: str = INK
    clock_weight: str = 'light'
    show_date: bool = True
    # The picture filled and darkened for each screen size, made once.
    scaled: dict = field(default_factory=dict, repr=False, compare=False)

    @classmethod
    def from_settings(cls, settings, picture=None):
        return cls(background=settings['background'], color=settings['backgroundColor'], color2=settings['backgroundColor2'],
                   picture=picture, dim=settings['pictureDim'], text=settings['textColor'], accent=settings['accentColor'],
                   clock_weight=settings['clockWeight'], show_date=settings['showDate'])


@dataclass(frozen=True)
class Palette:
    """What the screen paints with, each a ('#rrggbb', alpha) pair."""
    ink: tuple
    soft: tuple
    dim: tuple
    faint: tuple
    field: tuple
    field_over: tuple
    raised: tuple
    raised_over: tuple
    raised_down: tuple
    border_over: tuple
    border_typed: tuple
    border_typed_over: tuple
    initials: tuple
    chrome_over: tuple
    chrome_down: tuple


def palette(style):
    """The usual look keeps its exact colors; otherwise everything is a shade
    of the chosen text color, over a see-through field (Settings' preview
    uses the same rules)."""
    if style.background == 'default' and style.text.lower() == INK:
        return Palette(ink=(INK, 1), soft=(SOFT, 1), dim=(DIM, 1), faint=(FAINT, 1), field=(FIELD, 1), field_over=('#15171c', 1),
                       raised=(RAISED, 1), raised_over=('#2a2d34', 1), raised_down=('#1a1c21', 1), border_over=('#3a3e46', 1),
                       border_typed=('#4a4f58', 1), border_typed_over=('#5b616b', 1), initials=('#c9ccd2', 1),
                       chrome_over=('#22252b', 1), chrome_down=('#15171b', 1))
    text = style.text
    # Light text on a field that darkens what is behind it, dark text on one that lightens it.
    shade = '#000000' if luminance(text) > .45 else '#ffffff'
    lift = 0 if shade == '#000000' else .2
    return Palette(ink=(text, 1), soft=(text, .68), dim=(text, .45), faint=(text, .16), field=(shade, .3 + lift), field_over=(shade, .38 + lift),
                   raised=(text, .1), raised_over=(text, .18), raised_down=(text, .06), border_over=(text, .28),
                   border_typed=(text, .4), border_typed_over=(text, .55), initials=(text, .85),
                   chrome_over=(text, .14), chrome_down=(text, .07))


def rgb(value, alpha=1):
    return tuple(int(value[i:i+2], 16) / 255 for i in (1, 3, 5)) + (alpha,)


def tone(color, alpha=1):
    """cairo's (r, g, b, a) for '#rrggbb' or a palette's ('#rrggbb', alpha)."""
    if isinstance(color, tuple):
        color, alpha = color[0], color[1] * alpha
    return rgb(color, alpha)


def mix(color, other, amount):
    a, b = rgb(color), rgb(other)
    return '#' + ''.join(f'{round((x + (y - x) * amount) * 255):02x}' for x, y in zip(a[:3], b[:3]))


def luminance(color):
    channels = [c / 12.92 if c <= .03928 else ((c + .055) / 1.055) ** 2.4 for c in rgb(color)[:3]]
    return .2126 * channels[0] + .7152 * channels[1] + .0722 * channels[2]


def arrow_color(accent):
    """A dark arrow on a light accent, a white one on a dark accent."""
    return '#0b0c0e' if luminance(accent) > .45 else '#ffffff'


def accent_fill(accent, pressed, hovered):
    """The unlock button once something is typed: lighter under the pointer, darker pressed."""
    if accent.lower() == INK:
        return '#c9ccd2' if pressed else '#ffffff' if hovered else INK
    return mix(accent, '#000000', .18) if pressed else mix(accent, '#ffffff', .18) if hovered else accent


def rounded(ctx, x, y, w, h, r):
    r = min(r, w / 2, h / 2)
    ctx.new_sub_path()
    for cx, cy, start in ((x+w-r, y+r, -math.pi/2), (x+w-r, y+h-r, 0), (x+r, y+h-r, math.pi/2), (x+r, y+r, math.pi)):
        ctx.arc(cx, cy, r, start, start + math.pi/2)
    ctx.close_path()


def text(ctx, value, x, y, size, color=INK, width=420, align='left', weight=400, alpha=1):
    """Draws one line; returns its logical width in the current user units."""
    import gi
    gi.require_version('Pango', '1.0')
    gi.require_version('PangoCairo', '1.0')
    from gi.repository import Pango, PangoCairo
    layout = PangoCairo.create_layout(ctx)
    desc = Pango.FontDescription('Noto Sans')
    scale = abs(ctx.get_matrix().yy) or 1
    desc.set_absolute_size(max(size, 11 / scale) * Pango.SCALE)
    desc.set_weight(weight)
    layout.set_font_description(desc)
    layout.set_width(int(width * Pango.SCALE))
    layout.set_ellipsize(Pango.EllipsizeMode.END)
    layout.set_single_paragraph_mode(True)
    layout.set_alignment({'center': Pango.Alignment.CENTER, 'right': Pango.Alignment.RIGHT}.get(align, Pango.Alignment.LEFT))
    layout.set_text(value, -1)
    ctx.set_source_rgba(*tone(color, alpha))
    ctx.move_to(x - width / 2 if align == 'center' else x - width if align == 'right' else x, y)
    PangoCairo.show_layout(ctx, layout)
    ctx.new_path()  # The next shape must not start with a line from here.
    return layout.get_pixel_extents()[1].width


def icon(ctx, name, cx, cy, size, color):
    """Line icons on a 24-unit grid centred on (cx, cy)."""
    import cairo
    ctx.save(); ctx.translate(cx - size/2, cy - size/2); ctx.scale(size/24, size/24)
    ctx.set_source_rgba(*tone(color)); ctx.set_line_width(1.7)
    ctx.set_line_cap(cairo.LINE_CAP_ROUND); ctx.set_line_join(cairo.LINE_JOIN_ROUND)
    if name == 'suspend':
        ctx.push_group()
        ctx.set_source_rgba(*tone(color)); ctx.arc(12, 12, 7.5, 0, 2*math.pi); ctx.fill()
        ctx.set_operator(cairo.OPERATOR_CLEAR); ctx.arc(15.6, 8.6, 6.4, 0, 2*math.pi); ctx.fill()
        ctx.pop_group_to_source(); ctx.paint()
    elif name == 'reboot':
        end = math.radians(240)
        ctx.arc(12, 12, 7, math.radians(-30), end); ctx.stroke()
        # Filled head at the open end, pointing on along the circle.
        px, py = 12 + 7*math.cos(end), 12 + 7*math.sin(end)
        tx, ty = -math.sin(end), math.cos(end)
        nx, ny = math.cos(end), math.sin(end)
        ctx.move_to(px + tx*3.6, py + ty*3.6)
        ctx.line_to(px - tx*1.2 + nx*3, py - ty*1.2 + ny*3)
        ctx.line_to(px - tx*1.2 - nx*3, py - ty*1.2 - ny*3)
        ctx.close_path(); ctx.fill()
    elif name == 'shutdown':
        ctx.arc(12, 13, 7, math.radians(-55), math.radians(235)); ctx.stroke()
        ctx.move_to(12, 4); ctx.line_to(12, 12); ctx.stroke()
    elif name == 'arrow':
        ctx.move_to(6, 12); ctx.line_to(18, 12)
        ctx.move_to(13, 7); ctx.line_to(18, 12); ctx.line_to(13, 17); ctx.stroke()
    elif name == 'padlock':
        rounded(ctx, 5.5, 10.5, 13, 10, 2); ctx.stroke()
        ctx.arc(12, 10.5, 4, math.pi, 2*math.pi); ctx.stroke()
    ctx.restore()


def battery_icon(ctx, x, y, size, percent, charging, color):
    ctx.save(); ctx.translate(x, y); ctx.scale(size/24, size/24)
    ctx.set_source_rgba(*tone(color)); ctx.set_line_width(1.5)
    rounded(ctx, 2, 7, 18, 10, 2.2); ctx.stroke()
    rounded(ctx, 20.5, 10, 2, 4, .8); ctx.fill()
    ctx.set_source_rgba(*tone(DANGER if percent <= 10 and not charging else color))
    rounded(ctx, 4, 9, max(1.2, 14 * percent / 100), 6, 1); ctx.fill()
    ctx.restore()


def cursor(ctx, x, y, size=20):
    """The mouse pointer. XSecureLock's grab shows an invisible cursor, so
    the lock screen draws its own at the position it polls."""
    ctx.save(); ctx.translate(x, y); ctx.scale(size/20, size/20)
    ctx.move_to(0, 0); ctx.line_to(0, 16.5); ctx.line_to(4.1, 12.9); ctx.line_to(6.9, 19.2)
    ctx.line_to(9.5, 18.1); ctx.line_to(6.8, 11.9); ctx.line_to(12.2, 11.9); ctx.close_path()
    ctx.set_source_rgba(0, 0, 0, 1); ctx.set_line_width(1.3); ctx.stroke_preserve()
    ctx.set_source_rgba(1, 1, 1, 1); ctx.fill()
    ctx.restore()


CURSOR_BOX = (-2, -2, 18, 25)  # Area around the hotspot the pointer covers, at size 20.


def caret_box(width, height, state):
    """Where typing goes, as in any text field: after the last dot, or at the
    start of the empty field. Whole screen pixels (x, y, w, h), so the native
    screen can blink it without repainting the scene; None while checking."""
    if state.busy:
        return None
    s, top, cx, _ = metrics(width, height)
    dots = min(state.count, 16)
    # Clear of the placeholder's first letter while the field is empty.
    x = -131 + dots*13 if dots else -134
    return round(cx + x*s), round(top + 359*s), max(2, round(1.5*s)), round(18*s)


def caret(ctx, width, height, state):
    box = caret_box(width, height, state)
    if box:
        ctx.set_source_rgba(*tone(palette(state.style).ink)); ctx.rectangle(*box); ctx.fill()


def cursor_kind(target):
    """The system cursor for what is under the pointer: as on the desktop, a
    hand over buttons and a text cursor over the password field."""
    if target == 'field':
        return 'text'
    return 'pointer' if target else 'default'


def metrics(width, height):
    s = min(1.5, height / 820, width / 520)
    top = (height - 470 * s) / 2 - 40 * s
    cx = width / 2
    # The submit target shares exact coordinates with the painted arrow.
    button = (cx + 110*s, top + 351*s, 34*s, 34*s)
    return s, top, cx, button


def chrome_scale(width, height):
    """Corner controls stay readable on small screens and modest on 4K."""
    return max(.85, min(1.3, min(width, height * 1.6) / 1100))


def targets(width, height, state):
    s, top, cx, button = metrics(width, height)
    found = {'submit': button}
    hs = chrome_scale(width, height)
    size, gap, margin = 40 * hs, 12 * hs, 28 * hs
    x = width - margin - size
    for action in reversed([item for item in POWER_ACTIONS if item in state.actions]):
        found[action] = (x, height - margin - size, size, size)
        x -= size + gap
    # Last: the arrow inside it is matched first.
    found['field'] = (cx - 150*s, top + 345*s, 300*s, 46*s)
    return found


def hit(width, height, state, x, y):
    for name, (tx, ty, tw, th) in targets(width, height, state).items():
        if tx <= x < tx + tw and ty <= y < ty + th:
            return name
    return None


def cover(style, width, height):
    """The picture filling a width×height screen (cropped, never stretched),
    darkened; made once per screen size and reused for every repaint."""
    import cairo
    key = (int(width), int(height))
    found = style.scaled.get(key)
    if found is None:
        picture = style.picture
        pw, ph = picture.get_width(), picture.get_height()
        scale = max(key[0] / pw, key[1] / ph)
        found = cairo.ImageSurface(cairo.FORMAT_RGB24, max(1, key[0]), max(1, key[1]))
        c = cairo.Context(found)
        c.translate((key[0] - pw*scale) / 2, (key[1] - ph*scale) / 2); c.scale(scale, scale)
        c.set_source_surface(picture, 0, 0); c.get_source().set_filter(cairo.FILTER_GOOD); c.paint()
        c.identity_matrix()
        c.set_source_rgba(0, 0, 0, max(0, min(80, style.dim)) / 100); c.paint()
        style.scaled[key] = found
    return found


def background(ctx, width, height, style, cx, glow_y):
    import cairo
    if style.background == 'color':
        ctx.set_source_rgba(*rgb(style.color)); ctx.paint()
        return
    if style.background == 'gradient':
        bg = cairo.LinearGradient(0, 0, 0, height)
        bg.add_color_stop_rgb(0, *rgb(style.color)[:3])
        bg.add_color_stop_rgb(1, *rgb(style.color2)[:3])
        ctx.set_source(bg); ctx.paint()
        return
    if style.background == 'picture' and style.picture is not None and style.picture.get_width() and style.picture.get_height():
        ctx.set_source_surface(cover(style, width, height), 0, 0); ctx.paint()
        return
    bg = cairo.LinearGradient(0, 0, 0, height)
    bg.add_color_stop_rgb(0, *rgb('#0a0b0d')[:3])
    bg.add_color_stop_rgb(1, *rgb('#060708')[:3])
    ctx.set_source(bg); ctx.paint()
    # One faint, neutral light behind the clock; the edges fall to black.
    radius = max(width, height) * .62
    glow = cairo.RadialGradient(cx, glow_y, 0, cx, glow_y, radius)
    glow.add_color_stop_rgba(0, *rgb('#20242c', .55))
    glow.add_color_stop_rgba(1, *rgb('#20242c', 0))
    ctx.set_source(glow); ctx.paint()


def paint(ctx, width, height, state):
    s, top, cx, _ = metrics(width, height)
    t = strings(state.language)
    places = targets(width, height, state)
    style = state.style
    p = palette(style)

    background(ctx, width, height, style, cx, top + 140*s)

    ctx.save(); ctx.translate(cx, top); ctx.scale(s, s)
    clock, suffix = clock_label(state.now, state.clock24h)
    clock_width = text(ctx, clock, 0, 26, 96, p.ink, width=520, align='center', weight=CLOCK_WEIGHT.get(style.clock_weight, 300))
    if suffix:
        text(ctx, suffix, clock_width/2 + 8, 92, 18, p.soft, width=60, weight=400)
    if style.show_date:
        text(ctx, date_label(state.now, state.language), 0, 160, 15, p.soft, width=520, align='center')

    ctx.set_source_rgba(*tone(p.raised)); ctx.arc(0, 262, 30, 0, 2*math.pi); ctx.fill()
    if state.photo is not None and state.photo.get_width() and state.photo.get_height():
        # The same picture as Settings ▸ Account, filling the circle.
        ctx.save()
        ctx.arc(0, 262, 30, 0, 2*math.pi); ctx.clip()
        side = min(state.photo.get_width(), state.photo.get_height())
        ctx.translate(-30, 232); ctx.scale(60 / side, 60 / side)
        ctx.set_source_surface(state.photo, -(state.photo.get_width() - side) / 2, -(state.photo.get_height() - side) / 2)
        ctx.paint()
        ctx.restore()
    else:
        initials = ''.join(part[0] for part in state.name.split()[:2]).upper() or 'O'
        text(ctx, initials, 0, 249, 18, p.initials, width=60, align='center', weight=500)
    ctx.set_source_rgba(*tone(p.faint)); ctx.set_line_width(1); ctx.arc(0, 262, 30, 0, 2*math.pi); ctx.stroke()
    if state.show_name:
        text(ctx, state.name, 0, 300, 17, p.ink, width=360, align='center', weight=500)

    active = state.count and not state.busy
    over_field = state.hover in ('field', 'submit')
    rounded(ctx, -150, 345, 300, 46, 23)
    ctx.set_source_rgba(*tone(p.field_over if over_field else p.field)); ctx.fill_preserve()
    border = p.border_typed_over if state.count and over_field else p.border_typed if state.count else p.border_over if over_field else p.faint
    ctx.set_source_rgba(*tone(border)); ctx.set_line_width(1.2 if over_field else 1); ctx.stroke()
    if state.count:
        for i in range(min(state.count, 16)):
            ctx.set_source_rgba(*tone(p.ink)); ctx.arc(-126 + i*13, 368, 3.3, 0, 2*math.pi); ctx.fill()
    else:
        text(ctx, t['checking'] if state.busy else state.prompt or t['password'], -128, 357, 14, p.dim, width=220)
    hovered = state.hover == 'submit'
    pressed = hovered and state.pressed == 'submit'
    fill = accent_fill(style.accent, pressed, hovered) if active else (p.raised_down if pressed else p.raised_over if hovered else p.raised)
    ctx.set_source_rgba(*tone(fill))
    ctx.arc(127, 368, 16 if pressed else 17, 0, 2*math.pi); ctx.fill()
    if hovered and not pressed:
        # A soft ring: the arrow is clickable.
        ctx.set_source_rgba(*tone(p.ink, .18)); ctx.set_line_width(1)
        ctx.arc(127, 368, 20, 0, 2*math.pi); ctx.stroke()
    if state.busy:
        angle = state.now.microsecond / 1_000_000 * 2*math.pi
        ctx.set_source_rgba(*tone(p.soft)); ctx.set_line_width(1.8)
        ctx.arc(127, 368, 7, angle, angle + math.pi*1.5); ctx.stroke()
    else:
        icon(ctx, 'arrow', 127, 368, 20, arrow_color(style.accent) if active else p.dim)

    if state.failed and state.caps:
        message, color = t['caps'], WARNING
    elif state.failed:
        message, color = t['failed'], DANGER
    elif state.caps:
        message, color = t['caps'], WARNING
    else:
        message, color = t['hint'], p.dim
    text(ctx, message, 0, 408, 12.5, color, width=360, align='center')
    ctx.restore()

    hs = chrome_scale(width, height)
    margin = 28 * hs
    base = height - margin - 20 * hs
    x = margin
    icon(ctx, 'padlock', x + 9*hs, base, 18*hs, p.dim)
    x += 26*hs
    if width > 560:
        x += text(ctx, t['locked'], x, base - 8*hs, 12*hs, p.dim, width=160*hs) + 22*hs
    if state.layout:
        x += text(ctx, state.layout.upper(), x, base - 8*hs, 12*hs, p.dim, width=80*hs, weight=500) + 22*hs
    if state.battery:
        percent, charging = state.battery
        battery_icon(ctx, x, base - 12*hs, 24*hs, percent, charging, p.dim)
        text(ctx, f'{percent}%' + (f' · {t["charging"]}' if charging and percent < 100 else ''), x + 30*hs, base - 8*hs, 12*hs, p.dim, width=160*hs)

    for action in POWER_ACTIONS:
        if action not in places:
            continue
        bx, by, bw, bh = places[action]
        armed = state.armed == action
        over = state.hover == action
        down = over and state.pressed == action
        if armed or over:
            fill = '#3a2125' if armed and down else '#2a1a1d' if armed else p.chrome_down if down else p.chrome_over
            ctx.set_source_rgba(*tone(fill)); ctx.arc(bx + bw/2, by + bh/2, bw/2 - (1 if down else 0), 0, 2*math.pi); ctx.fill()
            ctx.set_source_rgba(*tone(DANGER if armed else p.ink, .22 if armed else .12)); ctx.set_line_width(1)
            ctx.arc(bx + bw/2, by + bh/2, bw/2 - .5, 0, 2*math.pi); ctx.stroke()
        icon(ctx, action, bx + bw/2, by + bh/2, 20*hs, DANGER if armed else p.ink if over else p.soft)
    label = t[f'confirm-{state.armed}'] if state.armed in ('reboot', 'shutdown') else state.notice or (t[state.hover] if state.hover in POWER_ACTIONS and state.hover in places else '')
    if label and any(action in places for action in POWER_ACTIONS):
        right = width - margin
        text(ctx, label, right, height - margin - 66*hs, 12*hs, DANGER if state.armed or state.notice else p.soft, width=320*hs, align='right')
    return places
