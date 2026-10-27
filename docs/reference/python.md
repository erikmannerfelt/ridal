# Python API

Generated from the docstrings of the installed `ridal` package. The
{doc}`../guide/python` guide shows how the functions fit together.

```{eval-rst}
.. module:: ridal
```

## Reading and inspecting

```{eval-rst}
.. autofunction:: ridal.read

.. autofunction:: ridal.info
```

## Positions

See {ref}`ridal-coords`.

```{eval-rst}
.. autofunction:: ridal.coords
```

## Processing

```{eval-rst}
.. autofunction:: ridal.process

.. autofunction:: ridal.batch_process
```

## Rendering

```{eval-rst}
.. autofunction:: ridal.render
```

## Data from other tools

See {ref}`the guide <python-other-tools>`.

```{eval-rst}
.. autofunction:: ridal.write_netcdf

.. autofunction:: ridal.impdar.from_radardata
```

## Discovery

```{eval-rst}
.. data:: ridal.all_steps
   :type: list[str]

   The names of every processing step. :doc:`steps` describes each one.

.. data:: ridal.all_step_descriptions
   :type: dict[str, str]

   Each step's name mapped to its description, the same text as
   ``ridal steps --describe-all``.

.. data:: ridal.all_formats
   :type: list[str]

   The names of the supported radar formats.

.. data:: ridal.all_format_descriptions
   :type: dict[str, dict]

   Each format's name mapped to its description, its capabilities (whether
   Ridal can ``read`` and ``write`` it) and the file extensions it uses for
   the header, the data and the coordinates.

.. data:: ridal.version
   :type: str

   The installed version of Ridal. ``ridal.__version__`` is the same value.
```

## HTTP client

`ridal.client`, for talking to a Ridal server; see {doc}`../guide/python-client`.
It needs the `client` extra.

```{eval-rst}
.. module:: ridal.client

.. autoclass:: ridal.client.Client
   :members:

.. autoclass:: ridal.client.ProgressEvent

.. autofunction:: ridal.client.tqdm_progress
```

### Planning uploads

```{eval-rst}
.. autoclass:: ridal.client.Plan
   :members:

.. autoclass:: ridal.client.Record

.. autoclass:: ridal.client.Outcome
```

### Records

What the server answers. Each mirrors a schema in
{download}`openapi.json`, field for field.

```{eval-rst}
.. autoclass:: ridal.client.Health
.. autoclass:: ridal.client.Me
.. autoclass:: ridal.client.TokenSummary
.. autoclass:: ridal.client.Grant
.. autoclass:: ridal.client.SignedIn
.. autoclass:: ridal.client.Project
.. autoclass:: ridal.client.Catalog
   :members: by_id, to_pandas
.. autoclass:: ridal.client.Dataset
.. autoclass:: ridal.client.Axes
.. autoclass:: ridal.client.FeatureCollection
   :members: to_geopandas
.. autoclass:: ridal.client.InterpretationList
.. autoclass:: ridal.client.Interpretation
.. autoclass:: ridal.client.Document
.. autoclass:: ridal.client.Added
.. autoclass:: ridal.client.Saved
.. autoclass:: ridal.client.ConsequenceReport
.. autoclass:: ridal.client.DocumentConsequence
.. autoclass:: ridal.client.CarryReport
.. autoclass:: ridal.client.Displacement
.. autoclass:: ridal.client.Dropped
.. autoclass:: ridal.client.Clipped
.. autoclass:: ridal.client.ShapeChange
.. autoclass:: ridal.client.Staged
.. autoclass:: ridal.client.Replaced
.. autoclass:: ridal.client.CarriedView
.. autoclass:: ridal.client.Promoted
```

### Processing records

```{eval-rst}
.. autoclass:: ridal.client.StoredFile
.. autoclass:: ridal.client.HashedFile
.. autoclass:: ridal.client.SourceView
.. autoclass:: ridal.client.Source
.. autoclass:: ridal.client.SourceNames
.. autoclass:: ridal.client.Bundle
.. autoclass:: ridal.client.Part
.. autoclass:: ridal.client.SidecarOverrides
.. autoclass:: ridal.client.BundleFile
.. autoclass:: ridal.client.SourceDem
.. autoclass:: ridal.client.Recording
.. autoclass:: ridal.client.SidecarSummary
.. autoclass:: ridal.client.Dem
.. autoclass:: ridal.client.Draft
   :members: finished
.. autoclass:: ridal.client.StepProgress
.. autoclass:: ridal.client.Profile
   :members: current
.. autoclass:: ridal.client.ProfileVersion
.. autoclass:: ridal.client.Assignment
.. autoclass:: ridal.client.RadargramStatus
.. autoclass:: ridal.client.Batch
   :members: finished
.. autoclass:: ridal.client.BatchItem
.. autoclass:: ridal.client.PicksNotCarried
.. autoclass:: ridal.client.IngestPlan
   :members: ok
.. autoclass:: ridal.client.PlannedRecording
.. autoclass:: ridal.client.PlannedOutput
.. autoclass:: ridal.client.PlannedAntenna
.. autoclass:: ridal.client.AntennaOverride
.. autoclass:: ridal.client.Split
.. autoclass:: ridal.client.PlanProblem
```

### Exceptions

```{eval-rst}
.. autoexception:: ridal.client.RidalError
.. autoexception:: ridal.client.ServerError
.. autoexception:: ridal.client.BadRequest
.. autoexception:: ridal.client.Unauthorized
.. autoexception:: ridal.client.Forbidden
.. autoexception:: ridal.client.NotFound
.. autoexception:: ridal.client.Conflict
.. autoexception:: ridal.client.NotAProject
.. autoexception:: ridal.client.PreconditionFailed
.. autoexception:: ridal.client.PayloadTooLarge
.. autoexception:: ridal.client.TooManyRequests
.. autoexception:: ridal.client.Unavailable
.. autoexception:: ridal.client.TransportError
.. autoexception:: ridal.client.ProtocolError
.. autoexception:: ridal.client.ReplacementRefused
.. autoexception:: ridal.client.ProcessingFailed
.. autoexception:: ridal.client.FigureWarning
```

## Removed

```{eval-rst}
.. autofunction:: ridal.run_cli
```
