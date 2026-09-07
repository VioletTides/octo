namespace Octo.Services.LastFm;

/// <summary>
/// Picks where a tune-in starts inside a station snapshot. Every listen used to open
/// with the same three cached tracks in snapshot order, which is most of why a
/// station felt like the same handful of songs on repeat.
/// </summary>
public interface IRadioTuneInSelector
{
    /// <summary>Index into the station's candidates to start scanning for cached tracks.</summary>
    int Start(int candidateCount);
}

public sealed class RandomRadioTuneInSelector : IRadioTuneInSelector
{
    public int Start(int candidateCount) => candidateCount <= 0 ? 0 : Random.Shared.Next(candidateCount);
}
