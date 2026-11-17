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
    ("def", "A plasmoid is a blob of plasma that holds its shape. "
            "It lives wherever its electrons win: gaining energy faster than they lose it."),
    ("gas", "A radio field shakes them. They slam into atoms, knock more electrons free, and the gas glows."),
    ("curl", "A magnet curls electrons. In thin gas, each one turns about half a circle before it hits an atom."),
    ("swarm", "So under the magnet, electrons can't wander off. They keep their heat, "
              "and the plasma there is easier to keep lit."),
    ("metal", "The magnet is also metal, and metal crowds the electric field, the way the hand drew the thread. "
              "At this pressure that is the stronger effect, nearly three to one."),
    # the answer, on the footage that raised the question
    ("back1", "So that is where electrons win, and where the foot stays."),
    ("back2", "Drag the magnet, and that spot moves. The foot follows."),
    ("back3", "Flick it, and the spot outruns the foot, until the next lap."),
    ("back4", "Lift the magnet, and no spot is special. It races free."),
    ("sun", "And the sun? The same curl, on a scale bigger than the Earth: "
            "plasma strung along magnetic field lines."),
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
