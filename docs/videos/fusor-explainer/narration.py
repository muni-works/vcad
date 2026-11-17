"""The narration, one spoken line per entry: (id, text). Captions show the
same words. No "we": the lines explain the physics. Sources for every
number are in script.md.
"""
LINES = [
    # the payoff first, then the question
    ("open1", "Inside this steel ball, atoms are fusing. The same reaction that lights a star."),
    ("open2", "No magnets. No lasers. Just a wire cage, and thirty thousand volts."),
    ("ask", "So how does a cage of wire fuse atoms?"),
    # the problem
    ("fuel", "The fuel is deuterium: hydrogen with an extra neutron. Every nucleus carries a positive charge."),
    ("repel", "And two positive charges push apart. To fuse, two nuclei have to meet fast enough to touch."),
    # the machine
    ("vac", "So pump out the air, and let in a trace of deuterium gas: "
            "a hundred and fifty thousand times thinner than air."),
    ("well", "Put the cage at minus thirty thousand volts, and leave the wall at zero. "
             "Now the whole chamber is a hill, and the cage is the bottom."),
    ("fall", "Strip an electron from an atom of the gas. What is left is an ion, with a positive charge. "
             "So it falls."),
    ("speed", "In ninety billionths of a second, it is moving at seventeen hundred kilometers a second."),
    ("miss", "But the cage is seventy percent empty space. So most ions miss the wire, "
             "cross the middle, and climb the far side."),
    ("back", "Then they fall back in. About seven trips, before a wire finally catches them."),
    ("core", "Every trip crosses the center. So that is where the fast ions crowd together."),
    # what happens there
    ("hit", "Almost always, a fast ion just grabs an electron from the gas. "
            "Now it is neutral, the voltage can't hold it, and it flies off."),
    ("fuse", "But once in a long while, two nuclei meet head on, and fuse: into helium, "
             "and a neutron leaving at twenty thousand kilometers a second."),
    # the scale
    ("count", "A machine this size should make about two thousand neutrons a second."),
    ("power", "It takes three hundred watts to do it. The fusion gives back a few billionths of one."),
    ("why", "Nearly every ion ends on a wire or a wall, without ever fusing."),
    ("so", "So a fusor is no power plant. It is the simplest machine that fuses atoms: "
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
