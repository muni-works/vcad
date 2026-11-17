"""The narration, one spoken line per entry: (id, text). Captions show the
same words. No "we": the lines explain the physics. Sources for every
claim are in script.md.
"""
LINES = [
    ("sun1", "What the fuck is the sun?"),
    ("sun2", "It isn't fire. It's plasma: gas so hot that its atoms are torn apart, into loose electrons and ions."),
    ("jar1", "You can make the same stuff in a jar. A thread of light, alive in thin gas."),
    ("jar2", "It even reaches for a hand."),
    ("dish1", "And on this tabletop dish, a knot of plasma races in circles. A plasmoid."),
    ("dish2", "Bring a magnet close, and it locks on."),
    ("pay1", "Drag the magnet, and it follows, like it's on rails."),
    ("pay2", "Flick it, and it snaps off, then gets caught again on the next lap."),
    ("pay3", "Lift the magnet away, and it's free."),
    ("ask", "So what is a plasmoid? And why would a magnet grab it?"),
    ("def", "A plasmoid is a blob of plasma that holds its shape, and moves as one thing."),
    ("gas", "Here, a radio-frequency field shakes the loose electrons in thin gas. "
            "They slam into atoms, knock more electrons free, and the atoms glow as they settle."),
    ("rf", "The obvious guess: magnets push on electric currents. But this current flips direction "
           "millions of times a second. So the push flips too, and averages out to nothing."),
    ("curl", "What a magnet really does is curl electrons. Every moving electron gets bent into a circle. "
             "Between collisions it turns about half a circle, so it can't wander off."),
    ("thin", "That only works in thin gas. In normal air, an electron hits an atom long before it can curl."),
    ("arcs", "It's the same reason the sun's plasma traces arcs. It's following magnetic field lines."),
    ("live", "Now: a plasma only stays lit where its electrons gain energy faster than they lose it."),
    ("lever1", "A magnet tips that balance two ways. First, curled electrons leak heat slowly. "
               "So the gas beneath the magnet runs hotter, and is easier to keep lit."),
    ("lever2", "Second, the magnet is a lump of metal, and metal crowds the electric field beneath it."),
    ("hand", "That's what pulled the thread toward the hand."),
    ("foot", "So the plasmoid's foot settles where the plasma is easiest to keep lit. "
             "At this pressure, the electric effect is nearly three times the magnetic one. "
             "Thin the gas further, and the magnetic effect grows."),
    # the sign-off, spelled the way it should be said (captions: CAPTIONS)
    ("outro", "This video was made entirely with open source code. My voice is called ribbit. "
              "R, I, B, T, dot dev. Build your imagination. V cad dot I O."),
]

# Lines whose captions differ from the spoken words: [(caption, spoken words covered)].
# The end card itself shows "build your imagination" and "vcad.io".
CAPTIONS = {
    "outro": [("this video was made", 4), ("entirely with", 2), ("open source code.", 3),
              ("my voice is called", 4), ("ribt", 1), ("ribt.dev", 6)],
}
