# GPR in brief

This page defines the terms the rest of this documentation uses. It is not a
course in ground penetrating radar; for that, see a textbook such as Jol
(ed., 2009), *Ground Penetrating Radar: Theory and Applications*.

Antennas
: A transmitter sends a short radar pulse into the ground and a receiver
  records what comes back. They are usually a fixed distance apart, the
  **antenna separation**: centimetres for high frequencies, several metres
  for the lowest. Each antenna has a nominal **frequency**: low frequencies
  reach deeper, high frequencies resolve finer detail.

Trace and sample
: One recording at one position is a **trace**: the amplitude received over
  a short time window, measured at regular intervals, the **samples**. Moving
  the radar along a line and recording traces gives a **profile**.

Radargram
: The traces of a profile side by side, as an image: position along the
  profile across, time down.

Two-way travel time
: The time from the pulse leaving the transmitter to a reflection reaching
  the receiver, down and up again, in nanoseconds (TWTT).

Direct wave and time zero
: The first arrival in every trace is the pulse travelling straight from
  transmitter to receiver, through the air and the ground surface. Its
  arrival is **time zero**, where travel time is counted from. Finding it is
  the zero correction.

Velocity and depth
: Radar travels at about 0.3 m/ns in air and more slowly in the ground:
  0.168 m/ns in ice, less in wet sediments. Depth is half the travel time
  times the velocity, corrected for the antenna separation. One velocity for
  the whole profile is usually an approximation.

Reflections
: The pulse is partly reflected wherever the electrical properties of the
  ground change: at the bed of a glacier, the water table, a layer boundary.
  A continuous boundary shows as a line across the radargram, a
  **reflector**. A small object shows as a **hyperbola**, because the radar
  sees it before and after passing over it.

Processing
: A raw radargram mixes the reflections with the direct wave, antenna
  ringing, slow drift, noise and a strong loss of amplitude with depth.
  Processing removes or reduces those so that the reflections can be read,
  and converts time to depth. See {doc}`../guide/processing`.

Interpretation
: Tracing reflectors through the radargram, **picking**, to map the
  boundaries they come from. See {doc}`../guide/interpretation`.
