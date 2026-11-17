"""The narration, one spoken line per entry: (id, text). Captions show the
same words. No "we": the lines explain the physics. Sources for every
number are in script.md.
"""
LINES = [
    # the payoff first, then the question
    ("open1", "Inside this steel ball, atoms are fusing. The same kind of reaction that lights a star."),
    ("open2", "All it takes is a wire cage, and thirty thousand volts."),
    ("ask", "So how does a cage of wire fuse atoms?"),
    # the problem
    ("fuel", "The fuel is deuterium: hydrogen with an extra neutron."),
    ("repel", "Its nuclei are positive, so they push apart. To fuse, they have to meet fast enough to touch."),
    # the machine
    ("vac", "So pump out the air, and let in a trace of deuterium: "
            "a hundred and fifty thousand times thinner than air."),
    ("well", "Put the cage at minus thirty thousand volts, and the wall at zero. "
             "Now the chamber's a hill, and the cage is the bottom."),
    ("fall", "Strip an electron off a gas atom, and it's an ion: positive. So it falls."),
    ("speed", "Ninety billionths of a second later, it's doing seventeen hundred kilometers a second."),
    ("miss", "But the cage is seventy percent empty space. So most ions miss the wire, and climb the far side."),
    ("back", "Then they fall back in. With nothing in the way, that's about seven trips before a wire catches them."),
    ("core", "Every trip crosses the center, so that's where the fast ions crowd."),
    # what happens there
    ("hit", "But the gas is in the way. Almost always, a fast ion just grabs an electron from it. "
            "Now it's neutral, and the voltage can't hold it."),
    ("fuse", "But once in a long while, two nuclei meet head on, and fuse: into helium, "
             "and a neutron doing twenty thousand kilometers a second."),
    # the scale
    ("count", "A machine this size should make about two thousand neutrons a second."),
    ("power", "That takes three hundred watts. The fusion gives back a few billionths of one."),
    ("why", "Nearly every ion ends on a wire or a wall, and never fuses."),
    ("so", "So a fusor isn't a power plant. It's the simplest machine that fuses atoms: "
           "a hill of voltage, a cage full of holes, and ions falling through the middle."),
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
