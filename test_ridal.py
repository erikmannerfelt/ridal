import ridal
import matplotlib.pyplot as plt

def main():

    print(ridal.version)
    filepath = "~/GPR/2025/GPR_20250325_A-Dronbreen-100MHz/DAT_0007_A1/DAT_0007_A1.rad"
    # obj = ridal.process(filepath, return_dataset=True, steps=["zero_corr", "bandpass", "siglog(1)", "correct_topography"], metadata={"a": 1})
    obj = ridal.read(filepath, return_dataset_format="xarray")
    data = obj

    # data["data_topographically_corrected"].plot()
    # plt.ylim(plt.gca().get_ylim()[::-1])
    # plt.show()


if __name__ == "__main__":
    main()
