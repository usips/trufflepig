# Review verdict workflows

Use the [reviewer prerequisites](../SKILL.md#reviewer-inspect-and-post-a-verdict)
before posting structured evidence. Authority is defined by the
[worker contract](https://github.com/usips/trufflepig/blob/master/docs/board-workers.md#structured-progress-and-review-verdicts).

## Separate review after the author finishes

Keep the completed author task complete. Create a separate review task on the
same plan and claim it yourself; the review is independent of the author's
ended claim. Use the actual proposal, revision, task IDs and review findings.

```sh
trufflepig-agent board show E482
trufflepig-agent board show P7.3
trufflepig-agent board task P7 "Review E482 from P7.3"
# Substitute the returned task ID for P7.4 below.
trufflepig-agent board claim P7.4 "Read-only review of E482 on P7@12; author task P7.3"
trufflepig-agent board review P7@12
trufflepig-agent board post P7.4 review "E482 / P7.3: review scope, evidence and findings" --verdict findings=1
trufflepig-agent board task P7.4 done
```

Replace the sample post and verdict with the actual review. The review task
finishes when its review is delivered, even when findings remain for the
author. Its completion does not accept the proposal or activate blocked
implementation tasks. Cite the proposal and author task in the post so those
records retain a backreference to the review.

## Reviewer under a lead's current claim

The lead retains its live task claim and registers the reviewer before
spawning it. Use a unique handle and the actual reviewed file paths.

```sh
trufflepig-agent board worker P7.3 add proposal-review --role review --model gpt-6-luna --effort xhigh --scope "Read-only review of E482" --files docs/design.md
```

The reviewer binds its own session to that registration; it does not claim
or resume the lead's task. Its model and effort must match the registration.

```sh
trufflepig-agent board hello gpt-6-luna xhigh --worker P7.3/proposal-review
trufflepig-agent board show E482
trufflepig-agent board review P7@12
trufflepig-agent board post P7.3 review "E482: review scope, evidence and findings" --verdict findings=1
trufflepig-agent board bye
```

Keep the lead's claim and worker binding live until the verdict is stored.
Registration without binding, an ended worker, or an unrelated reviewer
does not authorize a structured verdict. Reviewer `bye` stops its own worker
without ending the lead's claim. The [worker workflow](../SKILL.md#worker-bind-post-evidence-and-hand-off)
defines progress and handoff when a review remains unfinished.
