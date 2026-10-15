# GPR in brief

This page defines the terms that the rest of the documentation uses. It is
not a course in ground penetrating radar (GPR), for which a textbook such as
Jol (ed., 2009), *Ground Penetrating Radar: Theory and Applications*, is a
much better place to start.

Antennas
: A transmitter sends a short radar pulse into the ground and a receiver
  records what comes back. The two are usually a fixed distance apart (the
  **antenna separation**), from centimeters for high-frequency antennas to
  several meters for the lowest frequencies. Each antenna has a nominal
  **frequency**, and the choice of frequency is a trade-off: lower
  frequencies penetrate deeper, while higher frequencies resolve finer
  detail.

Trace and sample
: One recording at one position is a **trace**, which holds the amplitude
  received over a short time window, measured at regular intervals called
  **samples**. Moving the radar along a line while recording traces gives a
  **profile**.

Radargram
: The traces of a profile drawn side by side as an image, with the position
  along the profile on the horizontal axis and time on the vertical axis.

Two-way travel time
: The time (in nanoseconds) from when the pulse leaves the transmitter until
  a reflection of it reaches the receiver, i.e. the time down and up again,
  abbreviated TWTT.

Direct wave and time zero
: The first arrival in every trace is the pulse traveling straight from the
  transmitter to the receiver, through the air and along the ground surface.
  Its arrival defines **time zero**, which travel time is counted from, and
  finding it is the job of the zero correction.

Velocity and depth
: Radar waves travel at about 0.3 m/ns in air and more slowly in the
  ground, for example around 0.168 m/ns in most glacier ice and much less
  in wet sediments. Depth is half the travel time multiplied by the
  velocity, corrected for the antenna separation. A single velocity for a
  whole profile is usually an approximation, since snow, firn, water and
  sediments all have different velocities.

Reflections
: Part of the pulse is reflected wherever the electrical properties of the
  ground change, for example at the bed of a glacier, the water table or a
  boundary between sediment layers. A continuous boundary appears as a line
  across the radargram (a **reflector**), while a small object appears as a
  **hyperbola**, because the radar sees the object before and after passing
  over it.

Processing
: A raw radargram mixes the reflections with the direct wave, antenna
  ringing, slow drift, noise and a strong loss of amplitude with depth.
  Processing reduces these so that the reflections can be read, and converts
  time to depth (see {doc}`../guide/processing`).

Interpretation
: Tracing reflectors through the radargram (**picking**) to map the
  boundaries they come from (see {doc}`../guide/interpretation`).
