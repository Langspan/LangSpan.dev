# LangSpan mission

This is the canonical statement of LangSpan's purpose, product hypothesis and
success measures. [Implementation status](IMPLEMENTATION_STATUS.md) records
what has been built and verified.

## Mission

Help teams collaborate at the speed their agents work, while people set
direction and judge the result.

Developers, their agents and supporting tools need to know about related work
while a feature is being built. LangSpan's job is to get that information to
the participant making the next relevant decision. People remain responsible
for priorities, design choices and review of the combined result.

## The problem

Two developers can divide a connected feature across several agents, yet each
agent sees only part of the work. One changes an interface while another builds
against its old form. An edit can merge cleanly and still break the feature;
another can repeat work a teammate has already started.

Developers then have to relay updates, reconcile the results or separate
ownership so tightly that useful parallel work stops. Our product premise is
that faster agent-assisted changes will make this coordination gap more
costly. How often it happens, and how much LangSpan helps, must be measured.

Knowing who is touching a file is a start. The receiving participant also needs
to know what the other work means for its task, how current the information is,
and where to inspect the change.

## Founder context

The owner described a high-priority, hackathon-like team effort where a capable
swarm user finished tasks before teammates could volunteer. Others moved to
documentation or unrelated work to avoid interference. One person's harness
carried most of the implementation while teammates mainly supplied feedback.
The missing capability was a way for several people to contribute connected
implementation work without guessing what was already underway.

The owner also described solo swarms in separate worktrees that defer conflicts
until integration. These experiences motivate the hypothesis. They are not
customer testimonials or evidence of market prevalence, time savings or a
measured LangSpan result.

## Product hypothesis

If relevant work reaches the receiving agent before it chooses an affected
action, the team can complete the same connected feature with less repair and
less manual relay, while independent work continues and review quality holds.

Shared coordination describes the information participants can obtain and
where it reaches their workflow. It does not mandate a hosted service or a
general agent orchestrator. Developers keep their tools and agent sessions;
LangSpan connects the work happening through them.

Participants should be able to choose broad areas from a shared epic or issue
list, point their own harnesses at the work, and remain aware as local plans and
edits overlap. Awareness must include uncommitted and unpushed work; waiting for
source commits or pushes leaves the original gap in place.

That awareness can travel as metadata about tasks and local activity without
copying unpublished source. SQLite can remain local while a shared transport
announces work separately from source pushes. This requirement does not select
a central database, streaming service or hosted architecture. Jira epics and
GitHub issues describe the intended task workflow; their integration with
LangSpan is not claimed as built.

Here, timely awareness means information arrives before the decision it could
change. A short delivery time alone does not establish that benefit. An update
that arrives after the edit, or distracts an unrelated session, can miss the
purpose even when the transport is fast.

## Elevator pitch

LangSpan is the open-source coordination layer for parallel agentic development.
It brings relevant peer work into existing coding-agent sessions, coordinates
supported local edits, and helps teams verify combined changes. The goal is
compatible parallel work with less avoidable repair and manual relay.
[Product positioning](positioning.md) owns the public message and claim boundaries.

## Internal planning form

Use this form for planning, research and acceptance criteria. Public advertising
uses ordinary prose and a concrete example, not this template.

| Prompt | Statement |
| --- | --- |
| As a | Developer building a connected feature with teammates and their coding agents. |
| I want | Relevant work and changes to reach my agent before it chooses an affected action. |
| So that | We can work in parallel with less avoidable repair and manual relay, while people retain direction and review. |
| We know this is true when | Comparable runs meet the outcome and quality criteria below, with retained evidence of delivery and the receiving agent's response. |

## What success means

Keep three questions separate: did the information arrive, did it affect an
observable action, and did the team finish with less avoidable work? A working
transport or a quoted briefing answers only the first.

| Measure | Evidence to retain |
| --- | --- |
| Information before the decision | The originating work, delivery record and receiving session's next relevant tool action, in order. Include late and missing updates. |
| A useful response | An observable read, changed edit, new check or choice of other work that responds to the specific update. Do not infer it from receipt alone. |
| Repair effort | Corrective edits and time spent fixing duplicated or incompatible work. Record why each correction was needed; planned iteration is separate. |
| Manual relay | Human messages spent copying task state or changes between sessions. Keep design discussion, decisions and review separate from this count. |
| Parallel progress | Time to a working combined result, progress on unrelated tasks, and waits or denials that blocked work unnecessarily. |
| Team participation | Whether several developers can make implementation progress on the connected feature. Record when someone steps aside because the others' work is unclear. |
| Quality and review | The same functional checks and review criteria in both runs, including regressions and effort needed to review the result. |

Compare repeated runs with and without LangSpan using the same tasks, starting
code, agent versions and permission settings. Keep failures and incomplete runs
with the successful ones. Report individual results and sample size; do not
turn a small demonstration into a broad speed or reliability claim.

A successful result reduces coordination-related repair or manual relay
without worsening the accepted result, human review or useful parallel work.
Compare repair and relay separately; a gain in one must not conceal an increase
in the other.
An agent responding to a notice is intermediate evidence, not a substitute for
that comparison. A noisier workflow or repeated unnecessary waits count against
the hypothesis even if every message was delivered.

## A two-developer example

Two developers are adding session refresh. One agent changes the handler in
`src/auth/session.ts`; the other's agent updates the login flow and its tests.
They are working on different files, but the behavior has to agree.

The intended LangSpan flow brings the first agent's related work into the
second agent's session before it selects the affected edit, even while the
first change is still local. The receiving agent can account for that work,
inspect the changed code when available, and adjust its next action. Both
developers contribute to the feature and review the combined result without
copying each status update between conversations.

This is the scenario to demonstrate, not a measured success claim. Retained
[Claude briefing evidence](evidence/m4-briefing/README.md) shows receipt; it does
not establish a reduction in repair or manual relay. Use the measures above to
find out whether the shared information improves the completed feature.
