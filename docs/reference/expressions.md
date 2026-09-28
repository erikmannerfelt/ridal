# Derived item expressions

A **derived item** is a named expression over a project's picked layers, such
as a consensus bed from several people's picks, or the thickness between two
layers. This page describes the language those expressions are written in.
{doc}`../guide/interpretation` shows how to create and use them.

The language is a small, sandboxed subset of [Rhai](https://rhai.rs). Its
syntax is close to Rust's or JavaScript's, and nothing beyond what this page
describes is needed.

## How an expression is evaluated

The expression is evaluated separately at every position along the
radargram. Each evaluation must produce **one number**. The results, one per
position, make up the item.

Inside an expression, names mean the following:

A layer's id, such as {expr}`bed`
: The values every contributor has for that layer **at this position**: one
  value per contributor, in the item's unit. This is a list, not a number, so
  it has to be reduced to one number, for example with {expr}`median(bed)`. Each
  contributor's value has already been through the layer's reducer and its
  exclusivity rules (see {doc}`../guide/interpretation`). A contributor
  with no value here contributes {expr}`NaN`.

Another derived item's id
: That item's value at this position, already a single number, in this
  item's unit.

{expr}`NaN`
: "No value". It propagates through arithmetic, and every reduction skips it.

Whose picks count as "every contributor" depends on who is looking. An
`operator` or `admin` sees everyone's. Anyone else sees only their own, so
{expr}`median(bed)` is simply their own {expr}`bed`, unless an administrator has released
the item's result computed over everyone's picks.

## Layers and attributes

An item is a **derived layer** if its result is a position: a depth that can
be drawn as a line, and is available in metres, nanoseconds and samples. It
is a **derived attribute** if its result is some other number per position,
such as a thickness, a spread or a count. You never declare which; it is
inferred from the expression:

| Expression | Result |
|---|---|
| A layer id | layer |
| {expr}`median(x)`, {expr}`mean(x)`, {expr}`min(x)`, {expr}`max(x)`, {expr}`percentile(x, p)` | the same as `x` |
| {expr}`count(x)`, {expr}`std(x)`, {expr}`nmad(x)` | attribute |
| {expr}`a - b`, where both are layers | attribute (a distance between two positions) |
| {expr}`a + b`, {expr}`a - b` otherwise | layer if either side is one, otherwise attribute |
| {expr}`a * b`, {expr}`a / b` | attribute |
| {expr}`a * 2`, {expr}`2 * a`, {expr}`a / 2` and so on, with a plain number | the same as `a` |
| {expr}`shallowest(a, b)`, {expr}`deepest(a, b)`, {expr}`clamp(a, lo, hi)`, {expr}`abs(a)` | the same as `a` |
| {expr}`where(cond, a, b)` | the same as `a` (or `b`, if `a` is a plain number) |

So {expr}`median(bed)` is a derived layer, {expr}`median(bed) - median(surface)` is a
thickness and therefore an attribute, and {expr}`median(bed) + 2` is a layer
shifted two units down. A plain number and an attribute count the same on
either side of `+` and `-`: {expr}`10 - median(bed)` is also a layer, the bed
mirrored about 10 units.

A derived layer may lie outside the radargram, above its first sample or
below its last. {expr}`median(bed) + 5`, for a bed depth corrected by a known
error in time zero, is a real depth whether or not the recording reaches it.
Its value is kept as the expression gives it, and it is drawn off the edge of
the radargram rather than along it. To convert it into the other two units,
the depth and time axes are extended past their ends at their spacing
there. Its sample number can then be negative, or larger than the number of
samples.

## Units

Every item is written in one unit:

| Unit | Meaning |
|---|---|
| `meters` | Depth below the surface. The default. |
| `nanoseconds` | Two-way travel time. |
| `samples` | Sample number. May be fractional. |
| `dimensionless` | No unit: a count, a ratio or a flag. Only allowed for attributes. |

Depths, times and sample numbers are all **positive downwards**. Layer values
are converted into the item's unit before the expression runs. A derived
layer's result is converted back into the other two units, so that it can
be drawn and exported in any of them. An attribute is exported in its own
unit and never converted.

A derived layer is drawn as a line in the viewer. A derived attribute has no
line, so its value at the cursor's trace is shown in the viewer's cursor
readout instead: an item named "Bed pickers" with the expression
{expr}`count(bed)` reads as `Bed pickers: 4`. Every listed attribute is shown
there, and unticking "listed" removes it.

## Numbers

Every number is a decimal number: {expr}`50` and {expr}`50.0` are the same, so
{expr}`percentile(bed, 50)` and {expr}`bed > 50` work as written, and
{expr}`1 / 2` is {expr}`0.5`. A number may stand on either side of an operator:
{expr}`2 * median(bed)` is the same as {expr}`median(bed) * 2`.

## Functions

### Reductions

These turn a layer's per-contributor values into one number. All of them
skip {expr}`NaN`, and give {expr}`NaN` if nothing is left.

| Function | Result |
|---|---|
| {expr}`median(x)` | The median. With an even number of values, the mean of the middle two. |
| {expr}`mean(x)` | The arithmetic mean. |
| {expr}`min(x)`, {expr}`max(x)` | The smallest or largest value. Since depths grow downwards, {expr}`min` is the shallowest. |
| {expr}`percentile(x, p)` | The value at rank `floor(p / 100 × (n − 1))` among the `n` sorted values, with `p` from {expr}`0` to {expr}`100`. It never interpolates, so the result is always a value someone actually picked. |
| {expr}`count(x)` | How many contributors have a value. |
| {expr}`std(x)` | The sample standard deviation (dividing by `n − 1`). {expr}`NaN` with fewer than two values. |
| {expr}`nmad(x)` | The normalised median absolute deviation, 1.4826 times the median of the absolute differences from the median. A spread that is robust to outliers. |

### Combining layers

{expr}`concatenate(a, b)`
: Pools two layers' per-contributor values into one list, so that they can
  be reduced together: {expr}`median(concatenate(bed, bed_no_temperate))`. Layers
  are never pooled implicitly.

{expr}`a + b`, {expr}`a - b`, {expr}`a * b`, {expr}`a / b`
: Element-wise, contributor by contributor, when both sides are layers; a
  plain number applies to every contributor. {expr}`NaN` on either side gives
  {expr}`NaN`.

{expr}`shallowest(a, b)`, {expr}`deepest(a, b)`
: Element-wise minimum or maximum depth. {expr}`NaN` on either side gives {expr}`NaN`.

{expr}`clamp(a, lo, hi)`
: Limits `a` to the range from `lo` to `hi`. The bounds must be single
  numbers; reduce a layer first, as in {expr}`clamp(x, 0, median(bed))`.

{expr}`abs(a)`
: The absolute value, contributor by contributor for a layer.

### Conditions

Comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`) between a layer and a number,
or between two layers, give one result per contributor, which {expr}`where`
consumes:

{expr}`where(cond, a, b)`
: For each contributor, `a` where `cond` holds and `b` where it does not.
  A contributor with no value in `cond` gets {expr}`NaN`. For example,
  {expr}`median(where(bed > 50, bed, NaN))` ignores picks shallower than 50.

Comparing a layer with anything other than a number or another layer, such
as {expr}`bed > true`, is an error.

`if` works too, but only on a single true or false, such as a comparison
between two reduced values:
{expr}`if median(bed) > 100 { median(bed) } else { NaN }`. Using `if` on a
layer's per-contributor values is an error that points to {expr}`where`.

### Plain numbers

{expr}`min(a, b)` and {expr}`max(a, b)` of two single numbers give the smaller or
larger one, like {expr}`shallowest` and {expr}`deepest`. They do not take a layer;
use {expr}`shallowest` and {expr}`deepest` for that. {expr}`is_nan(a)` is true where a
single number is {expr}`NaN`. `let` can name an intermediate
value: {expr}`let b = median(bed); b - 2`.

## Limits

Expressions arrive over the network, so the language is deliberately
restricted. Loops (`for`, `while`, `loop`, `do`) and `eval` are not
available, and an expression may only run a small number of operations, to
a limited nesting depth.

## Examples

| Expression | Unit | What it is |
|---|---|---|
| {expr}`median(bed)` | `meters` | A consensus bed from everyone's picks. |
| {expr}`percentile(bed, 49)` | `meters` | A consensus bed that is always one contributor's actual pick. |
| {expr}`median(bed) - median(cts)` | `meters` | The thickness between two consensus layers. |
| {expr}`std(bed)` | `meters` | How much the contributors disagree about the bed. |
| {expr}`count(bed)` | `dimensionless` | How many people picked the bed here. |
| {expr}`median(concatenate(bed, bed_no_temperate))` | `meters` | A consensus over two layers that describe the same reflector in different conditions. |
| {expr}`clamp(median(bed) - median(cts), 0, 1000)` | `meters` | A thickness that is never negative. |

## Errors

An expression is checked when it is saved, and the editor's preview
evaluates it before anything is stored. An item can still break after it was
saved, for example when a layer it names is deleted. It then shows as invalid
in the viewer's layer panel and its error is shown as its kind on the Layers
page. It is left out of downloads and exports, and every other item keeps
working. The most common errors are:

"must reduce to a single value per position"
: The result is still one value per contributor. Wrap it in a reduction,
  such as {expr}`median()`.

"references missing layer" or "references missing derived item"
: A name is not a layer or derived item in the project.

"the derived items form a cycle"
: Items refer to each other in a loop, such as `a` using `b` and `b` using
  `a`.

"depends on '…', which cannot be evaluated"
: The item uses another derived item that has one of these errors itself.

"no function '…' accepts (…)"
: The name is misspelt, or the function does not take what it was given,
  such as {expr}`percentile(bed)` without a percentile, or {expr}`min(bed, 3)`,
  where {expr}`shallowest(bed, 3)` is meant. "No operator" is the same for `+`,
  `>` and the others.

"a condition must be a single true or false"
: `if` was given a layer's per-contributor values, as in `if bed > 50`. Use
  {expr}`where` to choose per contributor, or reduce first.
