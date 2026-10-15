# Citing Ridal

Ridal does not have a paper of its own yet, so to cite it, cite the software
together with the version you used, which `ridal --version` and
`ridal.version` print:

> Mannerfelt, E. S. and contributors: Ridal, version X.Y.Z,
> https://github.com/erikmannerfelt/ridal

```bibtex
@software{ridal,
  author  = {Mannerfelt, Erik Schytt and contributors},
  title   = {Ridal},
  version = {X.Y.Z},
  url     = {https://github.com/erikmannerfelt/ridal},
}
```

The version matters, since processing changes between versions. Every NetCDF
file that Ridal writes records the version that wrote it (`ridal_version`)
and how it was processed (`processing_steps`), and both are worth stating in
a methods section as well.

## Papers that have used Ridal

- Kleber et al. (2023): [Groundwater springs formed during glacial retreat are a large source of methane in the high Arctic](https://doi.org/10.1038/s41561-023-01210-6). *Nature Geoscience*.
- Harcourt et al. (2026): [Surging glaciers in Svalbard: Observing their distribution, characteristics and evolution](https://doi.org/10.1016/j.earscirev.2026.105410). *Earth-Science Reviews*.
- Kleber et al. (2026): [Subglacial geology and thermal conditions regulate methane emissions from Svalbard glaciers](https://doi.org/10.1038/s41467-026-77190-z). *Nature Communications*.

If you have published something that used Ridal,
[open an issue](https://github.com/erikmannerfelt/ridal/issues) or a pull
request to add it here.
