---
name: tempyr-interview
description: Use when the user wants to turn a brain dump, feature idea, or planning session into Tempyr graph nodes through the structured interview flow.
---

# Tempyr Interview Skill

Use this skill when the user is creating or refining Tempyr graph content through conversation.

## Core rules

- You are the interviewer, not the interviewee.
- Never invent answers to fill gaps.
- Ask at most 3 focused questions per turn.
- Prefer updating or linking existing graph nodes over creating duplicates.

## Workflow

1. Start with `interview_start` using the user's own words.
2. Present any relevant existing graph context before asking questions.
3. Drive the session through the five phases in order: Discovery -> Product -> Technical -> Decomposition -> Review.
4. In each phase, let the returned typed gaps determine the next contextual follow-up questions until the phase is complete and the interview advances.
5. After each user answer, call `interview_answer` with their actual answer and show the new tentative nodes or links in compact form.
6. In Review, summarize the proposed nodes and links, then call `interview_commit` only after explicit approval.

## When to skip the interview

If the user only wants to add a single note, insight, or one-off edge, prefer the direct graph tools instead.
