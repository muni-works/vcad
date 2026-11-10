# Export controls and sanctions

vcad operates **no hosted services**. The hosted MCP server (`mcp.vcad.io`),
cloud sync and accounts (Supabase), the server-side API under vcad.io, share
links, and the hosted render/inference spaces were removed from this
repository in vcad 1.0. What remains is software you run yourself:

- the open-source kernel, CLI and Mac app;
- the MCP server, which runs on your own machine (`npx -y @vcad/mcp`) with
  the agent of your choice;
- static files on vcad.io (documentation, downloads, the Mac app's update
  feed, and the retiring local-only web editor bundle). These carry no
  accounts, no server-side computation and no stored user data.

## The open-source code

This repository is published open-source software, which is not subject to
the EAR (15 C.F.R. §§ 734.3(b)(3), 734.7). Anyone may clone, build, and run
vcad locally under the [Apache License 2.0](../LICENSE).

## Your responsibility

You are responsible for complying with all export control and sanctions laws
that apply to you, including when you use vcad to design or manufacture
controlled items.
